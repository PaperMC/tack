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

use std::backtrace::Backtrace;
use thiserror::Error;

pub const ONLY_USE_AOT_FAILED_EXIT_CODE: u8 = 33;

#[derive(Debug, Error)]
pub enum Error {
    #[error("JNI error: {0:?}")]
    Jni(#[from] jni::errors::Error, #[backtrace] Backtrace),

    #[error("JVM error: {0:?}")]
    JVM(#[from] jni::JvmError, #[backtrace] Backtrace),

    #[error("JVM start error: {0:?}")]
    StartJvm(#[from] jni::errors::StartJvmError, #[backtrace] Backtrace),

    #[error("Failed to find Java installation: {0:?}")]
    JavaLoc(#[from] java_locator::errors::JavaLocatorError, #[backtrace] Backtrace),

    #[error("IO error: {0:?}")]
    Io(#[from] std::io::Error, #[backtrace] Backtrace),

    #[error("Time error: {0:?}")]
    Time(#[from] std::time::SystemTimeError, #[backtrace] Backtrace),

    #[cfg(not(target_os = "linux"))]
    #[error("Download error: {0:?}")]
    Net(#[from] nyquest::Error, #[backtrace] Backtrace),

    #[cfg(target_os = "linux")]
    #[error("Download error: {0:?}")]
    Net(#[from] curl::Error, #[backtrace] Backtrace),

    #[error("Zip error: {0:?}")]
    Zip(#[from] zip::result::ZipError, #[backtrace] Backtrace),

    #[error("UTF-8 error: {0:?}")]
    Utf(#[from] std::string::FromUtf8Error, #[backtrace] Backtrace),

    #[error("Hex error: {0:?}")]
    Hex(#[from] hex::FromHexError, #[backtrace] Backtrace),

    #[error("Parse error: {0:?}")]
    Int(#[from] std::num::ParseIntError, #[backtrace] Backtrace),

    #[error("Serialization error: {0:?}")]
    Postcard(#[from] postcard::Error, #[backtrace] Backtrace),

    #[error("{msg}\nCaused by: {cause}")]
    Wrapped {
        msg: String,
        #[source]
        #[backtrace]
        cause: Box<Error>,
    },

    #[error("{msg}")]
    Generic {
        msg: String,
        #[backtrace]
        backtrace: Backtrace,
    },

    #[error("Exiting with code {0}")]
    Exit(u8),
}

impl Error {
    pub fn generic(msg: impl Into<String>) -> Self {
        Self::Generic {
            msg: msg.into(),
            backtrace: Backtrace::capture(),
        }
    }

    pub fn wrap(msg: impl Into<String>, err: impl Into<Error>) -> Self {
        Self::Wrapped {
            msg: msg.into(),
            cause: Box::new(err.into()),
        }
    }
}

pub trait IntoErrorMsg {
    fn into_error_msg(self) -> String;
}
impl IntoErrorMsg for &'static str {
    fn into_error_msg(self) -> String {
        self.to_string()
    }
}
impl IntoErrorMsg for String {
    fn into_error_msg(self) -> String {
        self
    }
}
impl<F, S> IntoErrorMsg for F
where
    F: FnOnce() -> S,
    S: Into<String>,
{
    fn into_error_msg(self) -> String {
        self().into()
    }
}

pub trait WithContext<E: Into<Error>> {
    type Output;

    fn err_ctx(self, msg: impl IntoErrorMsg) -> Self::Output;
}
impl<T, E: Into<Error>> WithContext<E> for Result<T, E> {
    type Output = Result<T, Error>;

    fn err_ctx(self, msg: impl IntoErrorMsg) -> Self::Output {
        self.map_err(|e| Error::wrap(msg.into_error_msg(), e))
    }
}

pub trait MapErrGeneric<E> {
    type Output;

    fn map_err_generic<S: Into<String>>(self, f: impl FnOnce(E) -> S) -> Self::Output;
}
impl<T, E> MapErrGeneric<E> for Result<T, E> {
    type Output = Result<T, Error>;

    fn map_err_generic<S: Into<String>>(self, f: impl FnOnce(E) -> S) -> Self::Output {
        self.map_err(|e| Error::generic(f(e)))
    }
}

pub trait IntoError<T> {
    fn into_error(self) -> Result<T, Error>;
}
impl<T, E: Into<Error>> IntoError<T> for Result<T, E> {
    fn into_error(self) -> Result<T, Error> {
        self.map_err(|e| e.into())
    }
}

#[macro_export]
macro_rules! generic {
    ($($arg:tt)*) => {
        Err($crate::errors::Error::generic(format!($($arg)*)))
    };
}
