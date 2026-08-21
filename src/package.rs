use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use zip::{ZipArchive, ZipWriter, write::SimpleFileOptions};

use crate::{AlexError, load_app};

const INTEGRITY_PATH: &str = ".alex/integrity.json";
const MAX_PACKAGE_FILES: usize = 10_000;
const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 1024 * 1024 * 1024;

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
    #[error("invalid package id: {0}")]
    InvalidPackageId(String),
    #[error("application is not installed: {0}")]
    NotInstalled(String),
    #[error("package integrity check failed: {0}")]
    Integrity(String),
    #[error("package limit exceeded: {0}")]
    Limit(String),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct IntegrityManifest {
    algorithm: String,
    files: BTreeMap<String, String>,
}

#[derive(Debug)]
pub struct InstalledApp {
    pub id: String,
    pub name: String,
    pub version: String,
    pub path: PathBuf,
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
    let mut files = Vec::new();
    collect_files(source, source, output, &mut files)?;
    files.sort_by(|left, right| left.0.cmp(&right.0));
    let mut hashes = BTreeMap::new();
    for (relative, path) in &files {
        hashes.insert(relative.clone(), hash_reader(File::open(path)?)?);
    }
    let integrity = IntegrityManifest {
        algorithm: "sha256".into(),
        files: hashes,
    };

    let file = File::create(output)?;
    let mut writer = ZipWriter::new(file);
    for (relative, path) in files {
        writer.start_file(relative, SimpleFileOptions::default())?;
        io::copy(&mut File::open(path)?, &mut writer)?;
    }
    writer.start_file(INTEGRITY_PATH, SimpleFileOptions::default())?;
    writer.write_all(&serde_json::to_vec_pretty(&integrity).expect("integrity data is valid"))?;
    writer.finish()?;
    Ok(())
}

pub fn install(archive_path: &Path, install_root: &Path) -> Result<PathBuf, PackageError> {
    fs::create_dir_all(install_root)?;
    let temporary = tempfile::Builder::new()
        .prefix(".alex-install-")
        .tempdir_in(install_root)?;
    let mut archive = ZipArchive::new(File::open(archive_path)?)?;
    if archive.len() > MAX_PACKAGE_FILES + 1 {
        return Err(PackageError::Limit(format!(
            "more than {MAX_PACKAGE_FILES} files"
        )));
    }
    let integrity: IntegrityManifest = {
        let entry = archive
            .by_name(INTEGRITY_PATH)
            .map_err(|_| PackageError::Integrity("missing integrity manifest".into()))?;
        serde_json::from_reader(entry)
            .map_err(|error| PackageError::Integrity(format!("invalid manifest: {error}")))?
    };
    if integrity.algorithm != "sha256" {
        return Err(PackageError::Integrity(format!(
            "unsupported algorithm {}",
            integrity.algorithm
        )));
    }
    let mut seen = HashSet::new();
    let mut total_bytes = 0_u64;
    let mut integrity_entries = 0_usize;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let relative = entry
            .enclosed_name()
            .ok_or_else(|| PackageError::UnsafeEntry(entry.name().to_owned()))?;
        let relative_name = relative.to_string_lossy().replace('\\', "/");
        if relative_name == INTEGRITY_PATH {
            integrity_entries += 1;
            continue;
        }
        let identity = relative_name.to_ascii_lowercase();
        if !seen.insert(identity) {
            return Err(PackageError::Integrity(format!(
                "duplicate path {relative_name}"
            )));
        }
        if entry.size() > MAX_FILE_BYTES {
            return Err(PackageError::Limit(format!(
                "{relative_name} exceeds {MAX_FILE_BYTES} bytes"
            )));
        }
        total_bytes = total_bytes.saturating_add(entry.size());
        if total_bytes > MAX_TOTAL_BYTES {
            return Err(PackageError::Limit(format!(
                "expanded content exceeds {MAX_TOTAL_BYTES} bytes"
            )));
        }
        let destination = temporary.path().join(relative);
        if entry.is_dir() {
            fs::create_dir_all(&destination)?;
            continue;
        }
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut output = File::create(destination)?;
        let mut hasher = Sha256::new();
        let copied = copy_and_hash(&mut entry, &mut output, &mut hasher)?;
        if copied != entry.size() {
            return Err(PackageError::Integrity(format!(
                "size changed while extracting {relative_name}"
            )));
        }
        let actual = format!("{:x}", hasher.finalize());
        let expected = integrity
            .files
            .get(&relative_name)
            .ok_or_else(|| PackageError::Integrity(format!("unlisted file {relative_name}")))?;
        if &actual != expected {
            return Err(PackageError::Integrity(format!(
                "hash mismatch for {relative_name}"
            )));
        }
    }
    if integrity_entries != 1 {
        return Err(PackageError::Integrity(
            "archive must contain exactly one integrity manifest".into(),
        ));
    }
    if seen.len() != integrity.files.len() {
        return Err(PackageError::Integrity(
            "integrity manifest references missing files".into(),
        ));
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

pub fn list_installed(install_root: &Path) -> Result<Vec<InstalledApp>, PackageError> {
    if !install_root.exists() {
        return Ok(Vec::new());
    }
    let mut applications = Vec::new();
    for entry in fs::read_dir(install_root)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir()
            || path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with('.'))
        {
            continue;
        }
        if let Ok(manifest) = load_app(&path) {
            applications.push(InstalledApp {
                id: manifest.id,
                name: manifest.name,
                version: manifest.version,
                path,
            });
        }
    }
    applications.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(applications)
}

pub fn uninstall(package_id: &str, install_root: &Path) -> Result<PathBuf, PackageError> {
    if !valid_package_id(package_id) {
        return Err(PackageError::InvalidPackageId(package_id.to_owned()));
    }
    let root = install_root
        .canonicalize()
        .map_err(|_| PackageError::NotInstalled(package_id.to_owned()))?;
    let requested = root.join(package_id);
    let destination = requested
        .canonicalize()
        .map_err(|_| PackageError::NotInstalled(package_id.to_owned()))?;
    if destination.parent() != Some(root.as_path()) {
        return Err(PackageError::UnsafeEntry(destination.display().to_string()));
    }
    let manifest = load_app(&destination)?;
    if manifest.id != package_id {
        return Err(PackageError::InvalidPackageId(format!(
            "directory contains {}, not {package_id}",
            manifest.id
        )));
    }
    fs::remove_dir_all(&destination)?;
    Ok(destination)
}

fn collect_files(
    root: &Path,
    directory: &Path,
    output: &Path,
    files: &mut Vec<(String, PathBuf)>,
) -> Result<(), PackageError> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if path == output || ignored(&path) {
            continue;
        }
        if path.is_dir() {
            collect_files(root, &path, output, files)?;
        } else if path.is_file() {
            let relative = path
                .strip_prefix(root)
                .expect("walked path remains below package root")
                .to_string_lossy()
                .replace('\\', "/");
            files.push((relative, path));
        }
    }
    Ok(())
}

fn hash_reader(mut input: impl Read) -> Result<String, io::Error> {
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn copy_and_hash(
    input: &mut impl Read,
    output: &mut impl Write,
    hasher: &mut Sha256,
) -> Result<u64, io::Error> {
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        output.write_all(&buffer[..read])?;
        hasher.update(&buffer[..read]);
        total += read as u64;
    }
    Ok(total)
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

fn valid_package_id(id: &str) -> bool {
    id.contains('.')
        && id.split('.').all(|component| {
            !component.is_empty()
                && component.chars().all(|character| {
                    character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
                })
        })
}
