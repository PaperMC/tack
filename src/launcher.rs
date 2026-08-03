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

use crate::aot::check_aot_opt;
use crate::aot::meta::AotMeta;
use crate::aot::record::AotRecorder;
use crate::args::{ArgOptions, RecordMode};
use crate::classpath::{TackMeta, repo_dir, setup_classpath};
use crate::errors::{Error, MapErrGeneric, WithContext};
use crate::generic;
use crate::util::fs::{classpath_sep, try_delete_file};
use crate::util::jni::{JvmThreadDrop, check_java_version};
use crate::util::{JoinHandleRes, is_env_true};
use jni::objects::{JObjectArray, JString};
use jni::strings::JNIString;
use jni::{AttachConfig, Env, InitArgsBuilder, JNIVersion, JavaVM, jni_sig, jni_str};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::thread::{Scope, ScopedJoinHandle};

pub struct Launcher {
    pub jar: PathBuf,
    pub repo_dir: PathBuf,
    pub classpath: Vec<PathBuf>,
    pub tack_meta: TackMeta,
    pub java_home: String,
    pub args: Args,
    pub record: RecordMode,
    pub action: AotCacheAction,
    pub compat: bool,
    pub aot_files: AotFiles,
    pub log_files: LogFiles,
}

impl Launcher {
    pub fn builder(args: ArgOptions) -> Result<LauncherBuilder, Error> {
        let jar = Path::new(&args.jar);
        if !jar.exists() {
            return generic!("Jar file does not exist: {}", jar.display());
        }

        let repo_dir = repo_dir();
        let meta = AotMeta::aot_meta_file(&repo_dir);
        let cache = AotMeta::aot_cache_file(&repo_dir);
        let conf = AotMeta::aot_conf_file(&repo_dir);

        Ok(LauncherBuilder {
            jar: jar.to_path_buf(),
            repo_dir,
            classpath: None,
            tack_meta: None,
            java_home: None,
            args: Args {
                jvm: args.jvm_args,
                app: args.app_args,
            },
            record: args.record,
            action: None,
            compat: args.compat,
            aot_files: AotFiles { meta, cache, conf },
        })
    }

    pub fn check_aot_options(self) -> Result<Self, Error> {
        let action = check_aot_opt(&self)?;
        if action == self.action {
            return Ok(self);
        }

        Ok(Launcher {
            jar: self.jar,
            repo_dir: self.repo_dir,
            classpath: self.classpath,
            tack_meta: self.tack_meta,
            java_home: self.java_home,
            args: self.args,
            record: self.record,
            action,
            compat: self.compat,
            aot_files: self.aot_files,
            log_files: self.log_files,
        })
    }

    pub fn launch<'scope>(&'scope self, scope: &'scope Scope<'scope, '_>) -> Result<(), Error> {
        if is_env_true("TACK_PATCHONLY") || is_env_true("PAPERCLIP_PATCHONLY") {
            return Err(Error::Exit(0));
        }

        if self.action.is_record() {
            fn delete(p: &Path, name: &str) -> Result<(), Error> {
                if p.exists() {
                    std::fs::remove_file(p).err_ctx(|| format!("Failed to delete existing AOT {name} file"))?;
                }
                Ok(())
            }
            // If we're recording, we need to delete any existing AOT cache files
            delete(&self.aot_files.meta, "meta")?;
            delete(&self.aot_files.cache, "cache")?;
            delete(&self.aot_files.conf, "config")?;
        }

        let jvm_thread = self.start_background_thread(scope);

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

    fn start_background_thread<'scope>(
        &'scope self,
        scope: &'scope Scope<'scope, '_>,
    ) -> ScopedJoinHandle<'scope, Result<(), Error>> {
        scope.spawn(move || -> Result<(), Error> {
            let _hold = JvmThreadDrop; // Run drop() on this whenever this thread finishes

            match self.action {
                AotCacheAction::Use => try_delete_file(&self.log_files.use_aot),
                AotCacheAction::Record => try_delete_file(&self.log_files.record),
                AotCacheAction::None => {}
            }
            if let Err(e) = std::fs::create_dir_all(&self.log_files.dir) {
                let dir = &self.log_files.dir.display();
                eprintln!("Failed to create AOT logs directory ({dir}): {e}");
            }

            let jvm = self.init_jvm().err_ctx("Failed to create JVM")?;

            if self.action.is_record() {
                println!("Beginning AOT cache recording (This may cause slowdowns while the JVM is recording)...");
            }

            std::thread::scope(|scope| -> Result<(), Error> {
                let server_thread = self.start_jvm_thread(scope, &jvm).join_res();

                let meta_thread_res = match self.action {
                    AotCacheAction::Record => AotRecorder::new(&self, &jvm)
                        .start_record_thread(scope)
                        .map(|h| h.join_res()),
                    _ => None,
                };

                unsafe {
                    if let Err(e) = jvm.destroy() {
                        eprintln!("Error during JVM shutdown: {:?}", e);
                    }
                }

                if let Some(Err(ref e)) = meta_thread_res {
                    eprintln!("Error during AOT recording: {e}");
                }

                server_thread.err_ctx("Error during server thread")
            })
        })
    }

    fn init_jvm(&self) -> Result<JavaVM, Error> {
        let mut init_args = InitArgsBuilder::new().version(JNIVersion::V21);
        init_args = init_args
            .option("--enable-native-access=ALL-UNNAMED")
            .option("--sun-misc-unsafe-memory-access=allow");

        if self.action.is_record() || self.action.is_use() {
            init_args = init_args.option("-Xlog:aot*=off");
        }

        match self.action {
            AotCacheAction::Use => match self.aot_files.cache.to_str() {
                Some(aot_cache_file) => {
                    init_args = init_args
                        .option("-Xlog:aot*=info:file=.paper/logs/aot-use.log")
                        .option(format!("-XX:AOTCache={aot_cache_file}"));
                }
                None => {
                    return generic!(
                        "Failed to convert AOT cache file path to string: {}",
                        self.aot_files.cache.display()
                    );
                }
            },
            AotCacheAction::Record => match self.aot_files.conf.to_str() {
                Some(aot_conf_file) => {
                    init_args = init_args.option("-Xlog:aot*=info:file=.paper/logs/aot-record.log");
                    if self.compat {
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
                        self.aot_files.conf.display()
                    );
                }
            },
            AotCacheAction::None => {}
        }

        let classpath_text = self.classpath.iter().map(|p| p.as_os_str()).collect::<Vec<&OsStr>>();
        let classpath_text = classpath_text.join(&classpath_sep());
        let classpath_text = classpath_text
            .into_string()
            .map_err_generic(|e| format!("Failed to convert classpath to string: {}", e.display()))?;
        init_args = init_args.option(format!("-Djava.class.path={classpath_text}"));

        for arg in self.args.jvm.iter() {
            init_args = init_args.option(arg);
        }

        let init_args = init_args.build().err_ctx("Failed to initialize JVM")?;
        JavaVM::new(init_args).err_ctx("Failed to start JVM")
    }

    fn start_jvm_thread<'scope>(
        &'scope self,
        scope: &'scope Scope<'scope, '_>,
        jvm: &'scope JavaVM,
    ) -> ScopedJoinHandle<'scope, Result<(), Error>> {
        scope.spawn(|| -> Result<(), Error> {
            jvm.attach_current_thread_with_config(
                || AttachConfig::default().scoped(true).thread_name(jni_str!("main")),
                None,
                |env| -> Result<(), Error> { self.exec_jvm(env) },
            )
        })
    }

    fn exec_jvm(&self, env: &mut Env) -> Result<(), Error> {
        let args_array = JObjectArray::<JString>::new(env, self.args.app.len(), JString::null())?;
        for (i, arg) in self.args.app.iter().enumerate() {
            let arg = JString::new(env, arg.clone())?;
            args_array.set_element(env, i, arg)?;
        }

        let class_name = JNIString::new(self.tack_meta.main_class.replace(".", "/"));
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
}

pub struct LauncherBuilder {
    pub jar: PathBuf,
    pub repo_dir: PathBuf,
    pub classpath: Option<Vec<PathBuf>>,
    pub tack_meta: Option<TackMeta>,
    pub java_home: Option<String>,
    pub args: Args,
    pub record: RecordMode,
    pub action: Option<AotCacheAction>,
    pub compat: bool,
    pub aot_files: AotFiles,
}

impl LauncherBuilder {
    pub fn build(self) -> Result<Launcher, Error> {
        let dir = std::env::current_dir().err_ctx("Failed to get current directory")?;
        let logs_dir = dir.join(".paper").join("logs");
        let record = logs_dir.join("aot-record.log");
        let create = logs_dir.join("aot-create.log");
        let use_aot = logs_dir.join("aot-use.log");

        Ok(Launcher {
            jar: self.jar,
            repo_dir: self.repo_dir,
            classpath: self.classpath.ok_or(Error::Exit(1))?,
            tack_meta: self.tack_meta.ok_or(Error::Exit(1))?,
            java_home: self.java_home.ok_or(Error::Exit(1))?,
            args: self.args,
            record: self.record,
            action: self.action.unwrap_or(AotCacheAction::None),
            compat: self.compat,
            aot_files: self.aot_files,
            log_files: LogFiles {
                dir: logs_dir,
                record,
                create,
                use_aot,
            },
        })
    }

    pub fn check_java_version(&mut self) -> Result<(), Error> {
        let java_home = java_locator::locate_java_home().err_ctx("Failed to locate Java home")?;
        check_java_version(&java_home)?;
        self.java_home = Some(java_home);
        Ok(())
    }

    pub fn setup_classpath(&mut self) -> Result<(), Error> {
        let (classpath, meta) = setup_classpath(self)?;
        self.classpath = Some(classpath);
        self.tack_meta = Some(meta);
        Ok(())
    }
}

pub struct Args {
    pub jvm: Vec<String>,
    pub app: Vec<String>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum AotCacheAction {
    Use,
    Record,
    None,
}
impl AotCacheAction {
    pub fn is_use(self) -> bool {
        self == AotCacheAction::Use
    }

    pub fn is_record(self) -> bool {
        self == AotCacheAction::Record
    }

    pub fn is_none(self) -> bool {
        self == AotCacheAction::None
    }
}

pub struct AotFiles {
    pub meta: PathBuf,
    pub cache: PathBuf,
    pub conf: PathBuf,
}

pub struct LogFiles {
    pub dir: PathBuf,
    pub record: PathBuf,
    pub create: PathBuf,
    pub use_aot: PathBuf,
}
