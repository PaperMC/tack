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
pub mod launcher;
pub mod util;

use crate::args::split_args;
use crate::errors::{Error, WithContext};
use crate::launcher::Launcher;
use std::backtrace::{Backtrace, BacktraceStatus};
use std::error::request_ref;

include!(concat!(env!("OUT_DIR"), "/config.rs"));

fn main() {
    configure_networking();

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

    let mut launcher = Launcher::builder(arg_opts)?;
    launcher.check_java_version()?;
    launcher.setup_classpath().err_ctx("Failed to setup classpath")?;
    let launcher = launcher.build()?;

    let launcher = launcher.check_aot_options().err_ctx("Failed to check AOT cache")?;

    std::thread::scope(|s| launcher.launch(s))
}

#[cfg(not(target_os = "linux"))]
fn configure_networking() {
    nyquest_preset::register();
}

#[cfg(target_os = "linux")]
fn configure_networking() {
    #[cfg(feature = "rustls")]
    unsafe {
        openssl_probe::try_init_openssl_env_vars();
    }
    curl::init();
}
