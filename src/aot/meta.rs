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
use crate::launcher::Launcher;
use crate::util::fs::{create_directory, file_hash};
use crate::util::jni::java_bin;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

const AOT_CACHE_FILE: &str = "paper.aot";
const AOT_CONF_FILE: &str = "paper.aot.config";
const AOT_CACHE_META: &str = "paper.aot.meta";

#[derive(Serialize, Deserialize, Debug, Eq, PartialEq)]
pub struct AotMeta {
    jvm_ident: String,
    jvm_args: Vec<String>,
    classpath: Vec<[u8; 32]>,
    aot_cache_hash: [u8; 32],
}

impl AotMeta {
    pub fn build(launcher: &Launcher) -> Result<Self, Error> {
        let java_home = Path::new(launcher.java_home.as_str());
        let java = java_bin(java_home);

        let output = Command::new(&java).arg("-Xinternalversion").output();
        let output = output.err_ctx(|| {
            let msg = "Failed to execute 'java -Xinternalversion' command";
            format!("{} ({})", msg, java.display())
        })?;
        let jvm_ident = String::from_utf8_lossy(&output.stdout).trim().to_string();

        let mut classpath_hashed = Vec::<[u8; 32]>::new();
        for path in launcher.classpath.iter() {
            let file = Path::new(path);
            if !file.exists() {
                return generic!(
                    "Cannot compute AOT info: classpath entry does not exist: {}",
                    path.display()
                );
            }

            let hash = file_hash(file)?;
            classpath_hashed.push(hash);
        }

        let aot_cache_hash = file_hash(&launcher.aot_files.cache)?;

        Ok(AotMeta {
            jvm_ident,
            jvm_args: launcher.args.jvm.to_vec(),
            classpath: classpath_hashed,
            aot_cache_hash,
        })
    }

    pub fn aot_files(dir: &Path) -> (PathBuf, PathBuf) {
        (AotMeta::aot_cache_file(dir), AotMeta::aot_meta_file(dir))
    }

    pub fn aot_cache_file(dir: &Path) -> PathBuf {
        dir.join(".paper").join("cache").join(AOT_CACHE_FILE)
    }

    pub fn aot_conf_file(dir: &Path) -> PathBuf {
        dir.join(".paper").join("cache").join(AOT_CONF_FILE)
    }

    pub fn aot_meta_file(dir: &Path) -> PathBuf {
        dir.join(".paper").join("cache").join(AOT_CACHE_META)
    }

    pub fn read(aot_meta_file: &Path) -> Result<Option<Self>, Error> {
        let bytes = std::fs::read(aot_meta_file)
            .err_ctx(|| format!("Failed to read AOT meta file: {}", aot_meta_file.display()))?;
        let meta = postcard::from_bytes::<Self>(&bytes).ok();
        Ok(meta)
    }

    pub fn write(&self, aot_meta_file: &Path) -> Result<(), Error> {
        let bytes = Vec::new();
        let bytes = postcard::to_extend(&self, bytes).err_ctx("Failed to serialize AOT meta file")?;
        if let Some(parent) = aot_meta_file.parent() {
            create_directory(parent)?;
        }
        let tmp_out = aot_meta_file.with_file_name("paper.aot.meta.tmp");
        std::fs::write(&tmp_out, &bytes).err_ctx(|| format!("Failed to write AOT meta file: {}", tmp_out.display()))?;
        std::fs::rename(&tmp_out, aot_meta_file).err_ctx(|| {
            format!(
                "Failed to rename AOT meta file: {} -> {}",
                tmp_out.display(),
                aot_meta_file.display()
            )
        })?;

        Ok(())
    }
}
