use std::{
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use thiserror::Error;
use zip::{ZipArchive, ZipWriter, write::SimpleFileOptions};

use crate::{AlexError, load_app};

#[derive(Debug, Error)]
pub enum PackageError {
    #[error(transparent)]
    Alex(#[from] AlexError),
    #[error("package I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("invalid .alex archive: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("unsafe archive entry: {0}")]
    UnsafeEntry(String),
    #[error("application is already installed: {0}")]
    AlreadyInstalled(PathBuf),
    #[error("invalid project name: {0}")]
    InvalidName(String),
}

pub fn create_project(destination: &Path, package_id: &str) -> Result<(), PackageError> {
    let name = destination
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| valid_name(value))
        .ok_or_else(|| PackageError::InvalidName(destination.display().to_string()))?;
    if destination.exists() {
        return Err(PackageError::AlreadyInstalled(destination.to_path_buf()));
    }
    fs::create_dir_all(destination.join("frontend"))?;
    fs::create_dir_all(destination.join("backend"))?;
    let manifest = serde_json::json!({
        "schemaVersion": 1,
        "id": package_id,
        "name": name,
        "version": "0.1.0",
        "frontend": { "entry": "frontend/index.html" },
        "backend": { "runtime": "node", "entry": "backend/index.js" },
        "permissions": [{ "name": "runtime.invoke" }]
    });
    fs::write(
        destination.join("manifest.json"),
        format!(
            "{}\n",
            serde_json::to_string_pretty(&manifest).expect("JSON value is valid")
        ),
    )?;
    fs::write(
        destination.join("frontend/index.html"),
        "<!doctype html><meta charset=\"utf-8\"><h1>Alex OS App</h1>\n",
    )?;
    fs::write(
        destination.join("backend/index.js"),
        "const readline=require('node:readline');\nreadline.createInterface({input:process.stdin}).on('line',line=>{const r=JSON.parse(line);process.stdout.write(JSON.stringify({protocol:1,id:r.id,result:{ok:true}})+'\\n')});\n",
    )?;
    load_app(destination)?;
    Ok(())
}

pub fn pack(source: &Path, output: &Path) -> Result<(), PackageError> {
    load_app(source)?;
    if output.exists() {
        return Err(PackageError::AlreadyInstalled(output.to_path_buf()));
    }
    if let Some(parent) = output.parent().filter(|path| !path.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let file = File::create(output)?;
    let mut writer = ZipWriter::new(file);
    add_directory(&mut writer, source, source, output)?;
    writer.finish()?;
    Ok(())
}

pub fn install(archive_path: &Path, install_root: &Path) -> Result<PathBuf, PackageError> {
    fs::create_dir_all(install_root)?;
    let temporary = tempfile::Builder::new()
        .prefix(".alex-install-")
        .tempdir_in(install_root)?;
    let mut archive = ZipArchive::new(File::open(archive_path)?)?;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let relative = entry
            .enclosed_name()
            .ok_or_else(|| PackageError::UnsafeEntry(entry.name().to_owned()))?;
        let destination = temporary.path().join(relative);
        if entry.is_dir() {
            fs::create_dir_all(&destination)?;
            continue;
        }
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut output = File::create(destination)?;
        io::copy(&mut entry, &mut output)?;
    }
    let manifest = load_app(temporary.path())?;
    let destination = install_root.join(&manifest.id);
    if destination.exists() {
        return Err(PackageError::AlreadyInstalled(destination));
    }
    let extracted = temporary.keep();
    fs::rename(extracted, &destination)?;
    Ok(destination)
}

fn add_directory(
    writer: &mut ZipWriter<File>,
    root: &Path,
    directory: &Path,
    output: &Path,
) -> Result<(), PackageError> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if path == output || ignored(&path) {
            continue;
        }
        if path.is_dir() {
            add_directory(writer, root, &path, output)?;
        } else if path.is_file() {
            let relative = path
                .strip_prefix(root)
                .expect("walked path remains below package root")
                .to_string_lossy()
                .replace('\\', "/");
            writer.start_file(relative, SimpleFileOptions::default())?;
            let mut input = File::open(path)?;
            let mut buffer = Vec::new();
            input.read_to_end(&mut buffer)?;
            writer.write_all(&buffer)?;
        }
    }
    Ok(())
}

fn ignored(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| matches!(name, ".git" | "target" | ".alex"))
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
}
