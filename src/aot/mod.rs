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

pub mod meta;
pub mod record;

use crate::aot::meta::AotMeta;
use crate::args::RecordMode;
use crate::errors::{Error, IntoError, ONLY_USE_AOT_FAILED_EXIT_CODE, WithContext};
use crate::launcher::{AotCacheAction, Launcher};
use crate::util::fs::create_directory;
use crate::{generic, null};
use jni::objects::{JObject, JObjectArray, JString};
use jni::{Env, JValue, jni_sig, jni_str};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

pub fn check_aot_opt(launcher: &Launcher) -> Result<AotCacheAction, Error> {
    if launcher.record == RecordMode::NoAot {
        return Ok(AotCacheAction::None);
    }

    let (aot_cache_file, aot_meta_file) = AotMeta::aot_files(&launcher.repo_dir);
    if let Some(parent) = aot_cache_file.parent() {
        create_directory(parent)?;
    }

    let saved_meta = if aot_cache_file.exists() && aot_meta_file.exists() {
        AotMeta::read(&aot_meta_file)?
    } else {
        None
    };

    let saved_meta = match saved_meta {
        Some(m) => m,
        None => {
            return match launcher.record {
                RecordMode::Normal | RecordMode::OnlyRecord | RecordMode::ForceRecord => Ok(AotCacheAction::Record),
                RecordMode::Check => Err(Error::Exit(1)),
                RecordMode::OnlyUse => {
                    eprintln!("No AOT cache found");
                    Err(Error::Exit(ONLY_USE_AOT_FAILED_EXIT_CODE))
                }
                RecordMode::NoRecord => Ok(AotCacheAction::None),
                RecordMode::NoAot => unreachable!(),
            };
        }
    };

    let current_meta = AotMeta::build(launcher)?;

    match launcher.record {
        RecordMode::Normal => {
            if current_meta == saved_meta {
                Ok(AotCacheAction::Use)
            } else {
                Ok(AotCacheAction::Record)
            }
        }
        RecordMode::Check => Err(Error::Exit(if current_meta == saved_meta { 0 } else { 1 })),
        RecordMode::OnlyUse => {
            if current_meta != saved_meta {
                eprintln!("AOT cache is invalid");
                Err(Error::Exit(ONLY_USE_AOT_FAILED_EXIT_CODE))
            } else {
                Ok(AotCacheAction::Use)
            }
        }
        RecordMode::NoRecord => {
            if current_meta != saved_meta {
                Ok(AotCacheAction::None)
            } else {
                Ok(AotCacheAction::Use)
            }
        }
        RecordMode::OnlyRecord => {
            if current_meta == saved_meta {
                Err(Error::Exit(0))
            } else {
                Ok(AotCacheAction::Record)
            }
        }
        RecordMode::ForceRecord => Ok(AotCacheAction::Record),
        RecordMode::NoAot => unreachable!(),
    }
}

fn check_logs_for_errors() -> Result<bool, Error> {
    let log_file = Path::new(".paper").join("logs").join("aot-record.log");
    if !log_file.exists() {
        return generic!("No AOT log file found");
    }

    try {
        let file = File::open(&log_file).into_error()?;
        let reader = BufReader::new(file);
        for line in reader.lines() {
            let line = line.into_error()?;
            if line.contains("[error  ]") {
                return Ok(false);
            }
        }
    }
    .err_ctx("Failed to check AOT logs for errors")?;

    Ok(true)
}

// Threading

fn check_threads_are_running(env: &mut Env) -> Result<bool, Error> {
    let thread_mx_bean = env
        .call_static_method(
            jni_str!("java/lang/management/ManagementFactory"),
            jni_str!("getThreadMXBean"),
            jni_sig!("()Ljava/lang/management/ThreadMXBean;"),
            &[],
        )?
        .l()?;
    if thread_mx_bean.is_null() {
        return null!("ManagementFactory", "getThreadMXBean()");
    }

    let all_threads = env
        .call_method(
            &thread_mx_bean,
            jni_str!("dumpAllThreads"),
            jni_sig!("(ZZI)[Ljava/lang/management/ThreadInfo;"),
            &[JValue::Bool(false), JValue::Bool(false), JValue::Int(0)],
        )?
        .l()?;
    let all_threads = env.cast_local::<JObjectArray>(all_threads)?;
    let len = all_threads.len(env)?;

    if len == 0 {
        return Ok(false);
    }

    let terminated = env
        .get_static_field(
            jni_str!("java/lang/Thread$State"),
            jni_str!("TERMINATED"),
            jni_sig!("Ljava/lang/Thread$State;"),
        )?
        .l()?;

    for i in 0..len {
        let thread_info = all_threads.get_element(env, i)?;
        if thread_info.is_null() {
            continue;
        }

        let thread_name = thread_name(env, &thread_info)?;
        if thread_name != "Server thread" {
            continue;
        }

        let thread_state = env
            .call_method(
                thread_info,
                jni_str!("getThreadState"),
                jni_sig!("()Ljava/lang/Thread$State;"),
                &[],
            )?
            .l()?;
        if thread_state.is_null() {
            return null!("ThreadInfo", "getThreadState()");
        }

        let is_terminated = env.is_same_object(&terminated, &thread_state)?;
        if !is_terminated {
            // At least one thread is still running
            return Ok(true);
        }
    }

    Ok(false)
}

fn thread_name(env: &mut Env, thread_info: &JObject) -> Result<String, Error> {
    let thread_name = env
        .call_method(
            thread_info,
            jni_str!("getThreadName"),
            jni_sig!("()Ljava/lang/String;"),
            &[],
        )?
        .l()?;
    if thread_name.is_null() {
        return null!("ThreadInfo", "getThreadName()");
    }
    let thread_name = env.cast_local::<JString>(thread_name)?;
    let thread_name = thread_name.mutf8_chars(env)?;
    let thread_name = thread_name.to_str();
    Ok(thread_name.to_string())
}
