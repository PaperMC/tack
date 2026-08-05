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

use crate::aot::meta::AotMeta;
use crate::aot::{check_logs_for_errors, check_threads_are_running};
use crate::errors::{Error, WithContext};
use crate::launcher::Launcher;
use crate::util::fs::try_delete_file;
use crate::util::jni::{JavaLog, LogKind, jni_attach_thread};
use crate::{generic, null};
use jni::{AttachConfig, JValue, JavaVM, ScopeToken, jni_sig, jni_str};
use std::io::Read;
use std::process::Command;
use std::thread::{Scope, ScopedJoinHandle};
use std::time::Duration;

pub struct AotRecorder<'a> {
    launcher: &'a Launcher,
    jvm: &'a JavaVM,
}

impl<'a> AotRecorder<'a> {
    pub fn new(launcher: &'a Launcher, jvm: &'a JavaVM) -> Self {
        Self { launcher, jvm }
    }

    pub fn start_record_thread(self, scope: &'a Scope<'a, '_>) -> Option<ScopedJoinHandle<'a, Result<(), Error>>> {
        if self.launcher.aot_files.meta.exists() {
            return None;
        }

        let meta_thread_handle = scope.spawn(move || -> Result<(), Error> { self.exec_meta_thread() });
        Some(meta_thread_handle)
    }

    pub fn exec_meta_thread(&self) -> Result<(), Error> {
        let res = self.record_aot_cache();
        if let Err(ref e) = res {
            eprintln!("Failed to write AOT cache: {e}");
        }

        if !self.launcher.record.is_record() {
            return Ok(());
        }
        // For only record, stop the server

        let _ = self.jvm.attach_current_thread_with_config(
            || {
                AttachConfig::default()
                    .scoped(true)
                    .thread_name(jni_str!("aot-shutdown-thread"))
            },
            None,
            |env| -> Result<(), Error> {
                let minecraft_server = env
                    .get_static_field(
                        jni_str!("net/minecraft/server/MinecraftServer"),
                        jni_str!("SERVER"),
                        jni_sig!("Lnet/minecraft/server/MinecraftServer;"),
                    )?
                    .l()?;
                if minecraft_server.is_null() {
                    return Ok(());
                }
                if res.is_err() {
                    let _ = env.set_field(
                        &minecraft_server,
                        jni_str!("abnormalExit"),
                        jni_sig!("Z"),
                        JValue::Bool(true),
                    );
                };
                env.call_method(
                    &minecraft_server,
                    jni_str!("halt"),
                    jni_sig!("(Z)V"),
                    &[JValue::Bool(true)],
                )?;
                Ok(())
            },
        );

        Ok(())
    }

    pub fn record_aot_cache(&self) -> Result<(), Error> {
        let watchdog_thread = jni_str!("org/spigotmc/WatchdogThread");
        let has_started_field = jni_str!("hasStarted");
        let has_started_sig = jni_sig!("Z");

        loop {
            std::thread::sleep(Duration::from_millis(100));
            // We don't stay attached, so if the server stops early, `jvm.destroy()` won't be deadlocked waiting
            // for us to release the last non-daemon thread.

            let mut scope = ScopeToken::default();
            let mut guard = jni_attach_thread(self.jvm, &mut scope, jni_str!("aot-cache-checker"))?;
            let env = guard.borrow_env_mut();

            // While we are waiting, we need to make sure the `main` and `Server thread` threads are still running.
            // If both threads are gone, that means the server has crashed.
            if !check_threads_are_running(env)? {
                return Ok(());
            }

            let has_started = env
                .get_static_field(watchdog_thread, has_started_field, &has_started_sig)?
                .z();
            match has_started {
                Ok(true) => break,
                Ok(false) => continue,
                Err(jni::errors::Error::JavaException) => {
                    env.exception_clear(); // ignore it
                    continue;
                }
                Err(e) => return Err(Error::from(e)),
            };
        }

        // Server has completed starting

        let ended_recording = self.end_aot_recording().err_ctx("Failed to end AOT cache recording")?;
        if ended_recording {
            self.create_aot_cache().err_ctx("Failed to create AOT cache")?;

            self.jvm.log(
                LogKind::Info,
                "AOT cache file written successfully, writing meta file...",
            );

            if !self.launcher.aot_files.cache.exists() {
                return generic!("No AOT cache file found after recording!");
            }

            let meta = AotMeta::build(self.launcher).err_ctx("Failed to generate AOT cache meta")?;

            let aot_meta_file = AotMeta::aot_meta_file(&self.launcher.repo_dir);
            meta.write(&aot_meta_file).err_ctx("Failed to write AOT cache meta")?;

            self.jvm.log(LogKind::Info, "AOT cache meta written successfully.");
            self.jvm.log(LogKind::Info, "AOT cache recording complete!");
        }

        Ok(())
    }

    pub fn end_aot_recording(&self) -> Result<bool, Error> {
        let mut scope = ScopeToken::default();
        let mut guard = jni_attach_thread(self.jvm, &mut scope, jni_str!("aot-cache-checker"))?;
        let env = guard.borrow_env_mut();

        self.jvm
            .log(LogKind::Info, "AOT cache recording ended. Writing AOT config file...");

        let aot_cache_bean_class = env.find_class(jni_str!("jdk/management/HotSpotAOTCacheMXBean"))?;
        // Trigger record
        let aot_cache_bean = env
            .call_static_method(
                jni_str!("java/lang/management/ManagementFactory"),
                jni_str!("getPlatformMXBean"),
                jni_sig!("(Ljava/lang/Class;)Ljava/lang/management/PlatformManagedObject;"),
                &[JValue::Object(&aot_cache_bean_class)],
            )?
            .l()?;
        if aot_cache_bean.is_null() {
            return null!("ManagementFactory", "getPlatformMXBean()");
        }

        let ended = env
            .call_method(&aot_cache_bean, jni_str!("endRecording"), jni_sig!("()Z"), &[])?
            .z()?;

        // Check if there were errors writing the AOT file
        let is_ok = check_logs_for_errors().unwrap_or_else(|e| {
            eprintln!("Failed to check AOT logs for errors: {e}");
            // We can't verify there was an error, so we'll assume it's okay.
            true
        });

        if ended && is_ok {
            self.jvm.log(LogKind::Info, "AOT cache config recorded successfully.");
        } else {
            let mut msg = "AOT cache file writing failed. Check logs at '.paper/logs/aot-record.log'.".to_string();
            if !self.launcher.compat {
                msg.push_str("\nYou can try using '--aot-compat'.")
            }
            self.jvm.log(LogKind::Error, &msg);
        }

        Ok(ended && is_ok)
    }

    pub fn create_aot_cache(&self) -> Result<(), Error> {
        // Recording is done, we need to create the AOT cache now
        // We do that by calling ourselves again with the right JVM args
        let current_exe = std::env::current_exe().err_ctx(|| "Failed to get path to current executable")?;
        let aot_conf_file = match self.launcher.aot_files.conf.to_str() {
            Some(f) => f,
            None => return generic!("Failed to convert AOT config file to String"),
        };
        let aot_cache_file = AotMeta::aot_cache_file(&self.launcher.repo_dir);
        let aot_cache_file = match aot_cache_file.to_str() {
            Some(f) => f,
            None => return generic!("Failed to convert AOT cache file to String"),
        };

        if self.launcher.log_files.create.exists() {
            try_delete_file(&self.launcher.log_files.create);
        }
        if !self.launcher.log_files.dir.exists() {
            let _ = std::fs::create_dir_all(&self.launcher.log_files.dir);
        }

        let mut cmd = Command::new(current_exe);
        cmd.args([
            "--no-aot",
            "-Xlog:aot*=off",
            "-Xlog:aot*=info:file=.paper/logs/aot-create.log",
            "-XX:AOTMode=create",
        ]);
        if self.launcher.compat {
            cmd.args(["-XX:+UnlockDiagnosticVMOptions", "-XX:-AOTInvokeDynamicLinking"]);
        }

        let jvm_args = &self.launcher.args.jvm;
        jvm_args.iter().filter(|arg| arg.starts_with("-XX:")).for_each(|arg| {
            cmd.arg(arg);
        });

        cmd.args([
            format!("-XX:AOTConfiguration={aot_conf_file}").as_str(),
            format!("-XX:AOTCache={aot_cache_file}").as_str(),
        ]);

        let jar = self.launcher.jar.to_str();
        let jar = jar.ok_or_else(|| Error::generic("Failed to convert jar path to String"))?;

        cmd.args(["-jar", jar]);

        let (mut reader, writer) = os_pipe::pipe()?;
        cmd.stdout(writer.try_clone()?);
        cmd.stderr(writer);
        let mut child = cmd.spawn().err_ctx("Failed to create child process")?;

        self.jvm.log(LogKind::Info, "Creating AOT cache file...");

        let exit_status = child.wait().err_ctx("Failed to wait for child process")?;
        drop(cmd); // drops both writers with it (will deadlock otherwise)

        if !exit_status.success() {
            let mut combined_output = String::new();
            reader.read_to_string(&mut combined_output)?;
            let mut msg = "Failed to record AOT cache. Check logs at '.paper/logs/aot-create.log'.".to_string();
            if !self.launcher.compat {
                msg.push_str("\nYou can try using '--aot-compat'.");
            }
            if !combined_output.is_empty() {
                msg.push('\n');
                msg.push_str(combined_output.trim());
            }
            self.jvm.log(LogKind::Error, &msg);
            return generic!("Failed to create AOT cache");
        }

        Ok(())
    }
}
