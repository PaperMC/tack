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

use crate::errors::{Error, WithContext};
use crate::generic;
use std::thread::ScopedJoinHandle;

pub mod fs;
pub mod jni;
#[cfg(windows)]
pub mod windows;

pub fn copy_owned<S: Clone>(slice: &[S]) -> Vec<S> {
    let mut res = Vec::with_capacity(slice.len());
    for item in slice {
        res.push(item.clone());
    }
    res
}

pub fn is_env_true(env: &str) -> bool {
    std::env::var_os(env).is_some_and(|prop| prop == "1" || prop.eq_ignore_ascii_case("true"))
}

pub fn parse_hex(s: &str) -> Result<[u8; 32], Error> {
    let decoded = hex::decode(s)?;
    if decoded.len() != 32 {
        return generic!("Invalid hex string (not 32 bytes long): {s}");
    }
    let mut res = [0u8; 32];
    res.copy_from_slice(&decoded);
    Ok(res)
}

pub fn parse_hex_named(name: &str, s: &str) -> Result<[u8; 32], Error> {
    parse_hex(s).err_ctx(|| format!("Failed to parse hex string '{name}': {s}"))
}

pub trait JoinHandleRes {
    type Output;
    fn join_res(self) -> Self::Output;
}

impl<'a, T> JoinHandleRes for ScopedJoinHandle<'a, Result<T, Error>> {
    type Output = Result<T, Error>;

    fn join_res(self) -> Self::Output {
        self.join().unwrap_or_else(|_| generic!("Thread error"))
    }
}

pub struct ComposingIterator<I> {
    base: Box<dyn Iterator<Item = I>>,
    layer: Option<Box<dyn Iterator<Item = I>>>,
}

impl<T> ComposingIterator<T> {
    pub fn new(base: Box<dyn Iterator<Item = T>>) -> Self {
        Self { base, layer: None }
    }

    pub fn push_layer(&mut self, layer: Box<dyn Iterator<Item = T>>) {
        self.layer = Some(layer);
    }

    pub fn is_nested(&self) -> bool {
        self.layer.is_some()
    }
}

impl<I> Iterator for ComposingIterator<I> {
    type Item = I;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(layer) = &mut self.layer {
            match layer.next() {
                Some(n) => return Some(n),
                None => self.layer = None,
            }
        }
        self.base.next()
    }
}
