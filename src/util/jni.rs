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

use crate::errors::{Error, IntoError, WithContext};
use crate::generic;
use jni::objects::{JObject, JPrimitiveArray, TypeArray};
use jni::strings::JNIStr;
use jni::{AttachConfig, AttachGuard, Env, JValue, JavaVM, ScopeToken, jni_sig, jni_str};
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn jni_attach_thread<'scope>(
    jvm: &JavaVM,
    scope: &'scope mut ScopeToken,
    thread_name: &JNIStr,
) -> jni::errors::Result<AttachGuard<'scope>> {
    unsafe { jvm.attach_current_thread_guard(|| AttachConfig::default().scoped(true).thread_name(thread_name), scope) }
}

pub struct JvmThreadDrop;
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

pub fn as_primitive_array<'a: 'b, 'b, T: TypeArray>(
    env: &mut Env<'a>,
    array: &[T],
) -> Result<JPrimitiveArray<'b, T>, Error> {
    let java_array = JPrimitiveArray::<T>::new(env, array.len())?;
    java_array.set_region(env, 0, array)?;
    Ok(java_array)
}

pub fn get_logger<'a>(env: &mut Env<'a>) -> Result<JObject<'a>, Error> {
    let logger_name = env.new_string("AOT")?;
    env.call_static_method(
        jni_str!("org/slf4j/LoggerFactory"),
        jni_str!("getLogger"),
        jni_sig!("(Ljava/lang/String;)Lorg/slf4j/Logger;"),
        &[JValue::Object(&logger_name)],
    )?
    .l()
    .into_error()
}

#[derive(Clone, Copy, Debug)]
pub enum LogKind {
    Info,
    Error,
}

pub trait JavaLog {
    fn log<S: AsRef<str>>(&self, kind: LogKind, msg: S);
}

impl JavaLog for JavaVM {
    fn log<S: AsRef<str>>(&self, kind: LogKind, msg: S) {
        let msg = msg.as_ref();
        let res = self.attach_current_thread_with_config(
            || {
                AttachConfig::default()
                    .scoped(true)
                    .thread_name(jni_str!("aot-cache-checker"))
            },
            None,
            |env| -> Result<(), Error> {
                let logger = get_logger(env)?;

                let method = match kind {
                    LogKind::Info => jni_str!("info"),
                    LogKind::Error => jni_str!("error"),
                };

                let bar = if let LogKind::Error = kind {
                    let star = "*";
                    let len = msg.lines().next().unwrap_or("").len();
                    let bar = star.repeat(len);
                    Some(env.new_string(bar)?)
                } else {
                    None
                };

                if let Some(ref bar) = bar {
                    env.call_method(
                        &logger,
                        method,
                        jni_sig!("(Ljava/lang/String;)V"),
                        &[JValue::Object(&bar)],
                    )?;
                }

                let message = env.new_string(msg)?;
                env.call_method(
                    &logger,
                    method,
                    jni_sig!("(Ljava/lang/String;)V"),
                    &[JValue::Object(&message)],
                )?;

                if let Some(ref bar) = bar {
                    env.call_method(
                        &logger,
                        method,
                        jni_sig!("(Ljava/lang/String;)V"),
                        &[JValue::Object(&bar)],
                    )?;
                }

                Ok(())
            },
        );
        if let Err(_) = res {
            // We failed to log the message, so just print it
            match kind {
                LogKind::Info => println!("{msg}"),
                LogKind::Error => eprintln!("{msg}"),
            }
        }
    }
}

#[macro_export]
macro_rules! null {
    ($class:literal, $method:literal) => {
        $crate::generic!("{}::{} returned 'null'", $class, $method)
    };
}

pub fn java_bin(base: &Path) -> PathBuf {
    let mut path = base.to_path_buf();
    path.push("bin");
    path.push("java");
    #[cfg(windows)]
    {
        path.set_extension("exe");
    }
    path
}

pub fn check_java_version(java_home: &str) -> Result<(), Error> {
    let java_home = Path::new(java_home);
    let java = java_bin(java_home);

    let version_text = Command::new(&java)
        .arg("-version")
        .output()
        .err_ctx(|| format!("Failed to execute 'java -version' command ({})", java.display()))?;
    let version_text = version_text.stderr;
    let version_text = String::from_utf8_lossy(&version_text);
    let version_line = match version_text.lines().next() {
        Some(l) => l,
        None => {
            return generic!("Failed to parse 'java -version' (no output)");
        }
    };

    let start = match version_line.chars().position(|c| c == '"') {
        Some(i) => i + 1, // skip the quote
        None => {
            return generic!("Failed to parse 'java -version' (no version number found)");
        }
    };

    let version_line = &version_line[start..];
    let version = match version_line.chars().position(|c| c == '"') {
        Some(i) => &version_line[..i],
        None => return generic!("Failed to parse 'java -version' (no version number found)"),
    };

    let version = version.strip_suffix("-ea").unwrap_or(version);
    let version_parts = version.split('.').take(3).collect::<Vec<&str>>();
    let major_version = version_part(&version_parts, 0)?;
    let minor_version = version_part(&version_parts, 1)?;
    let patch_version = version_part(&version_parts, 2)?;

    let version_err = "Unsupported Java version: Java 25.0.4 or higher is required, found: ";

    // We require >25.0.4
    if major_version.is_none() || major_version.unwrap() < 25 {
        let mut err_msg = format!(
            "{}{}",
            version_err,
            major_version
                .map(|v| v.to_string())
                .unwrap_or_else(|| "unknown".to_string())
        );
        if let (Some(minor_version), Some(patch_version)) = (minor_version, patch_version) {
            err_msg.push_str(&format!(".{}.{}", minor_version, patch_version));
        }
        eprintln!("{err_msg}");
        return Err(Error::Exit(1));
    }
    let major_version = major_version.unwrap();
    if major_version > 25 {
        return Ok(());
    }
    if minor_version.is_none() || patch_version.is_none() {
        // We can't verify it's 25.0.4, but we tried
        return Ok(());
    }
    let minor_version = minor_version.unwrap();
    let patch_version = patch_version.unwrap();
    if minor_version > 0 || patch_version >= 4 {
        return Ok(());
    }

    eprintln!("{}{}.{}.{}", version_err, major_version, minor_version, patch_version);
    Err(Error::Exit(1))
}

fn version_part(parts: &[&str], index: usize) -> Result<Option<u16>, Error> {
    if parts.len() <= index {
        return Ok(None);
    }
    let num = parts[index].parse::<u16>().err_ctx(|| {
        format!(
            "Failed to parse 'java -version' (invalid version number): {}",
            parts[index]
        )
    })?;
    Ok(Some(num))
}
