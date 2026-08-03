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
use digest_io::IoWrapper;
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use zip::ZipArchive;
use zip::read::ZipFile;
use zip::result::ZipError;

pub fn file_hash(path: &Path) -> Result<[u8; 32], Error> {
    try {
        let mut file = open_file(path)?;
        let mut digest = IoWrapper(Sha256::new());

        std::io::copy(&mut file, &mut digest).into_error()?;
        let mut hash: [u8; 32] = [0u8; 32];
        hash.copy_from_slice(&digest.0.finalize().0);
        hash
    }
    .err_ctx(|| format!("Failed to hash file {}", path.display()))
}

pub fn file_matches_hash(path: &Path, expected_hash: &[u8]) -> Result<bool, Error> {
    try {
        let mut file = open_file(path)?;
        let mut digest = IoWrapper(Sha256::new());

        std::io::copy(&mut file, &mut digest).into_error()?;
        let actual_hash = digest.0.finalize();

        expected_hash == actual_hash.0
    }
    .err_ctx("Failed to hash file")
}

pub fn bytes_matches_hash(data: &[u8], expected_hash: &[u8]) -> bool {
    let mut digest = Sha256::new();
    digest.update(data);

    let actual_hash = digest.finalize();
    expected_hash == actual_hash.0
}

pub fn extract_zip_entry(entry: &mut ZipFile<File>, internal_path: &str, destination: &Path) -> Result<(), Error> {
    if entry.is_dir() {
        create_directory(destination)?;
        return Ok(());
    }

    if let Some(parent) = destination.parent()
        && !parent.exists()
    {
        create_directory(parent)?;
    }

    let mut out_file = create_file(destination)?;
    std::io::copy(entry, &mut out_file).err_ctx(|| {
        let dest = destination.display();
        format!("Failed to extract {internal_path} from zip to {dest}")
    })?;

    Ok(())
}

pub fn create_directory(path: &Path) -> Result<(), Error> {
    std::fs::create_dir_all(path).err_ctx(|| format!("Failed to create directory: {}", path.display()))
}

pub fn write_file(path: &Path, data: &[u8]) -> Result<(), Error> {
    std::fs::write(path, data).err_ctx(|| format!("Failed to write file: {}", path.display()))
}

pub fn create_file(path: &Path) -> Result<File, Error> {
    File::create(path).err_ctx(|| format!("Failed to create file: {}", path.display()))
}

pub fn try_delete_file(path: &Path) {
    if path.exists() {
        if let Err(e) = std::fs::remove_file(&path) {
            let file_name = path.file_name().map(|p| p.to_string_lossy()).unwrap_or_default();
            eprintln!("Failed to delete existing {} file: {}", file_name, e);
        }
    }
}

pub fn open_file(path: &Path) -> Result<File, Error> {
    File::open(path).err_ctx(|| format!("Failed to open file: {}", path.display()))
}

pub fn open_zip(path: &Path) -> Result<ZipArchive<File>, Error> {
    ZipArchive::new(open_file(path)?).err_ctx(|| format!("Failed to open zip: {}", path.display()))
}

pub fn find_zip_entry<'a>(archive: &'a mut ZipArchive<File>, name: &str) -> Result<Option<ZipFile<'a, File>>, Error> {
    let entry = archive.by_name(name);
    match entry {
        Ok(entry) => Ok::<Option<ZipFile<'_, File>>, Error>(Some(entry)),
        Err(ZipError::FileNotFound) => Ok(None),
        Err(e) => return Err(Error::from(e)),
    }
    .err_ctx(|| format!("Failed to find entry in zip: {}", name))
}

pub fn require_zip_entry<'a>(archive: &'a mut ZipArchive<File>, name: &str) -> Result<ZipFile<'a, File>, Error> {
    match find_zip_entry(archive, name)? {
        Some(entry) => Ok(entry),
        None => generic!("Failed to find entry in zip: {name}"),
    }
}

pub fn read_zip_entry(entry: &mut ZipFile<File>) -> Result<Vec<u8>, Error> {
    let mut data = Vec::new();
    entry
        .read_to_end(&mut data)
        .err_ctx(|| format!("Failed to read entry: {}", entry.name()))?;
    Ok(data)
}

pub fn read_zip_entry_text(entry: &mut ZipFile<File>) -> Result<String, Error> {
    let data = read_zip_entry(entry)?;
    String::from_utf8(data).err_ctx(|| format!("Invalid UTF-8 in {}", entry.name()))
}

#[cfg(not(target_os = "windows"))]
pub fn classpath_sep() -> OsString {
    ":".into()
}
#[cfg(target_os = "windows")]
pub fn classpath_sep() -> OsString {
    ";".into()
}
