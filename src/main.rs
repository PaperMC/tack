/*
 * tack - Paper server launcher with AOT cache management.
 * Copyright (C) 2026  Kyle Wood (DenWav)
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, version 3 of the License only.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with this program.  If not, see <https://www.gnu.org/licenses/>.
 */

#![feature(try_blocks)]
#![feature(error_generic_member_access)]

pub mod aot;
pub mod args;
pub mod classpath;
pub mod errors;
pub mod jni;
pub mod util;

use crate::aot::{AotCacheAction, AotMeta, AotRecordingArgs, check_aot_opt, setup_auto_recording};
use crate::args::split_args;
use crate::classpath::{repo_dir, setup_classpath};
use crate::errors::{Error, MapErrGeneric, WithContext};
use crate::jni::check_java_version;
use crate::util::{JoinHandleRes, classpath_sep, copy_owned};
use ::jni::objects::{JObjectArray, JString};
use ::jni::strings::JNIString;
use ::jni::{AttachConfig, Env, InitArgsBuilder, JNIVersion, JavaVM, jni_sig, jni_str};
use std::backtrace::{Backtrace, BacktraceStatus};
use std::error::request_ref;
use std::ffi::OsString;
use std::path::Path;
use std::sync::Arc;
use std::thread::JoinHandle;

include!(concat!(env!("OUT_DIR"), "/config.rs"));

fn main() {
    nyquest_preset::register();
    match run() {
        // For Error::Exit we just want to exit, we don't want to print the error message
        Err(Error::Exit(code)) => std::process::exit(code as i32),
        Err(e) => {
            eprintln!("{e}");

            // If backtraces are enabled, print it
            if let Some(backtrace) = request_ref::<Backtrace>(&e) {
                if backtrace.status() == BacktraceStatus::Captured {
                    eprintln!("{}", backtrace);
                }
            };

            std::process::exit(1)
        }
        Ok(_) => {}
    }
}

fn run() -> Result<(), Error> {
    let args: Vec<String> = std::env::args().collect();
    let arg_opts = split_args(args)?.ok_or(Error::Exit(0))?;

    let jar = Path::new(&arg_opts.jar);
    if !jar.exists() {
        return generic!("Jar file does not exist: {}", jar.display());
    }

    let repo_dir = repo_dir();
    let jvm_args = arg_opts.jvm_args;
    let app_args = arg_opts.app_args;

    let java_home = java_locator::locate_java_home().err_ctx("Failed to locate Java home")?;
    check_java_version(&java_home)?;

    let (classpath, meta) = setup_classpath(&repo_dir, jar).err_ctx("Failed to setup classpath")?;

    let aot_action = check_aot_opt(
        &repo_dir,
        arg_opts.record,
        arg_opts.compat,
        &java_home,
        &classpath,
        &jvm_args,
        &app_args,
    )
    .err_ctx("Failed to check AOT cache")?;

    if let AotCacheAction::Record { .. } = aot_action {
        fn delete(p: &Path, name: &str) -> Result<(), Error> {
            if p.exists() {
                std::fs::remove_file(p)
                    .err_ctx(|| format!("Failed to delete existing AOT {name} file"))?;
            }
            Ok(())
        }
        // If we're recording, we need to delete any existing AOT cache files
        delete(&AotMeta::aot_meta_file(&repo_dir), "meta")?;
        delete(&AotMeta::aot_cache_file(&repo_dir), "cache")?;
        delete(&AotMeta::aot_conf_file(&repo_dir), "config")?;
    }

    let jvm_args = copy_owned(&jvm_args);
    let app_args = copy_owned(&app_args);
    let jvm_thread = std::thread::spawn(move || -> Result<(), Error> {
        let _hold = JvmThreadDrop; // Run drop() on this whenever this thread finishes

        {
            let aot_logs_dir = Path::new(".paper").join("logs");
            if aot_logs_dir.exists() {
                if aot_action.is_record() {
                    let create_log = aot_logs_dir.join("aot-record.log");
                    if create_log.exists()
                        && let Err(e) = std::fs::remove_file(&create_log)
                    {
                        eprintln!("Failed to delete existing aot-create.log file: {e}");
                    }
                } else if aot_action.is_use() {
                    let use_log = aot_logs_dir.join("aot-use.log");
                    if use_log.exists()
                        && let Err(e) = std::fs::remove_file(&use_log)
                    {
                        eprintln!("Failed to delete existing aot-use.log file: {e}");
                    }
                }
            } else if let Err(e) = std::fs::create_dir_all(&aot_logs_dir) {
                eprintln!("Failed to create AOT logs directory: {}", e);
            }
        }

        let jvm = create_jvm(&jvm_args, &classpath, &aot_action).err_ctx("Failed to create JVM")?;
        let jvm = Arc::new(jvm);

        if aot_action.is_record() {
            println!(
                "Beginning AOT cache recording (This may cause slowdowns while the JVM is recording)..."
            );
        }
        let server_thread = start_jvm_thread(jvm.clone(), &meta.main_class, &app_args);
        let server_thread_res = server_thread.join_res();

        let meta_thread_res = match aot_action {
            AotCacheAction::Record {
                aot_conf_file,
                compat,
            } => {
                let meta_thread = setup_auto_recording(AotRecordingArgs {
                    jvm: jvm.clone(),
                    java_home,
                    jar: arg_opts.jar,
                    repo_dir,
                    classpath,
                    jvm_args,
                    app_args,
                    mode: arg_opts.record,
                    aot_conf_file,
                    compat,
                });
                meta_thread.map(|h| h.join_res())
            }
            _ => None,
        };

        unsafe {
            if let Err(e) = jvm.destroy() {
                eprintln!("Error during JVM shutdown: {:?}", e);
            }
        }
        drop(jvm);

        if let Some(Err(ref e)) = meta_thread_res {
            eprintln!("Error during AOT recording: {e}");
        }

        server_thread_res.err_ctx("Error during server thread")
    });

    // Run the native macOS event loop on the main thread
    // This keeps the main thread unblocked and processes AWT window events
    #[cfg(target_os = "macos")]
    {
        use core_foundation::runloop::CFRunLoopRun;
        unsafe {
            // This will block and process UI events until canceled
            CFRunLoopRun();
        }
    }

    jvm_thread.join().unwrap_or(Err(Error::Exit(1)))
}

struct JvmThreadDrop;
impl Drop for JvmThreadDrop {
    fn drop(&mut self) {
        #[cfg(target_os = "macos")]
        {
            use core_foundation::runloop::{CFRunLoopGetMain, CFRunLoopStop};
            unsafe {
                CFRunLoopStop(CFRunLoopGetMain());
            }
        }
    }
}

fn start_jvm_thread(
    jvm: Arc<JavaVM>,
    main_class: &str,
    app_args: &[String],
) -> JoinHandle<Result<(), Error>> {
    let app_args = app_args
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<String>>();
    let main_class = main_class.to_string();

    std::thread::spawn(move || {
        jvm.attach_current_thread_with_config(
            || {
                AttachConfig::default()
                    .scoped(true)
                    .thread_name(jni_str!("main"))
            },
            None,
            |env| exec_jvm(env, &main_class, &app_args),
        )
    })
}

fn create_jvm(
    jvm_args: &[String],
    classpath: &[OsString],
    aot_action: &AotCacheAction,
) -> Result<JavaVM, Error> {
    fn is_env_true(env: &str) -> bool {
        std::env::var_os(env).is_some_and(|prop| prop.eq_ignore_ascii_case("true"))
    }
    if is_env_true("TACK_PATCHONLY") || is_env_true("PAPERCLIP_PATCHONLY") {
        return Err(Error::Exit(0));
    }

    init_jvm(jvm_args, classpath, aot_action)
}

fn init_jvm(
    jvm_args: &[String],
    classpath: &[OsString],
    aot_action: &AotCacheAction,
) -> Result<JavaVM, Error> {
    let mut init_args = InitArgsBuilder::new().version(JNIVersion::V21);
    init_args = init_args
        .option("--enable-native-access=ALL-UNNAMED")
        .option("--sun-misc-unsafe-memory-access=allow");

    match aot_action {
        AotCacheAction::Use { .. } | AotCacheAction::Record { .. } => {
            init_args = init_args.option("-Xlog:aot*=off");
        }
        AotCacheAction::None => {}
    }

    match aot_action {
        AotCacheAction::Use { aot_cache_file } => match aot_cache_file.to_str() {
            Some(aot_cache_file) => {
                init_args = init_args
                    .option("-Xlog:aot*=info:file=.paper/logs/aot-use.log")
                    .option(format!("-XX:AOTCache={aot_cache_file}"));
            }
            None => {
                return generic!(
                    "Failed to convert AOT cache file path to string: {}",
                    aot_cache_file.display()
                );
            }
        },
        AotCacheAction::Record {
            aot_conf_file,
            compat,
        } => match aot_conf_file.to_str() {
            Some(aot_conf_file) => {
                init_args = init_args.option("-Xlog:aot*=info:file=.paper/logs/aot-record.log");
                if *compat {
                    init_args = init_args
                        .option("-XX:+UnlockDiagnosticVMOptions")
                        .option("-XX:-AOTInvokeDynamicLinking");
                }
                init_args = init_args
                    .option("-XX:AOTMode=record")
                    .option(format!("-XX:AOTConfiguration={aot_conf_file}"));
            }
            None => {
                return generic!(
                    "Failed to convert AOT conf file path to string: {}",
                    aot_conf_file.display()
                );
            }
        },
        AotCacheAction::None => {}
    }

    let classpath_text = classpath.join(&classpath_sep());
    let classpath_text = classpath_text
        .into_string()
        .map_err_generic(|e| format!("Failed to convert classpath to string: {}", e.display()))?;
    init_args = init_args.option(format!("-Djava.class.path={classpath_text}"));

    for arg in jvm_args {
        init_args = init_args.option(arg);
    }

    let init_args = init_args.build().err_ctx("Failed to initialize JVM")?;
    JavaVM::new(init_args).err_ctx("Failed to start JVM")
}

fn exec_jvm(env: &mut Env, main_class: &str, args: &[String]) -> Result<(), Error> {
    let args_array = JObjectArray::<JString>::new(env, args.len(), JString::null())?;
    for (i, arg) in args.iter().enumerate() {
        let arg = JString::new(env, arg.clone())?;
        args_array.set_element(env, i, arg)?;
    }

    let class_name = JNIString::new(main_class.replace(".", "/"));
    let psvm_name = jni_str!("main");
    let psvm_desc = jni_sig!("([Ljava/lang/String;)V");
    env.call_static_method(class_name, psvm_name, psvm_desc, &[(&args_array).into()])?;

    if env.exception_check() {
        env.exception_describe(); // Prints the stack trace to stderr
        env.exception_clear();
        // Don't return any error here, the error message has already been printed
    }

    Ok(())
}
