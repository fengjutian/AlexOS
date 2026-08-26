//! Signed, versioned distribution store for llama.cpp and ONNX GenAI workers.

use super::{ModelError, WorkerDescriptor};
use base64::Engine as _;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};
use zip::ZipArchive;

const MAX_FILES: usize = 4096;
const MAX_BYTES: u64 = 8 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum WorkerEngine {
    LlamaCpp,
    OnnxRuntimeGenai,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkerPackageManifest {
    pub schema_version: u32,
    pub engine: WorkerEngine,
    pub worker_kind: String,
    pub version: String,
    pub triple: String,
    pub url: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub publisher_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkerPackageRequest {
    pub manifest: WorkerPackageManifest,
    pub signature: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct InstalledWorkerPackage {
    pub engine: WorkerEngine,
    pub worker_kind: String,
    pub version: String,
    pub triple: String,
    pub root: PathBuf,
    pub active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ActiveVersion {
    version: String,
    triple: String,
}

#[derive(Clone)]
pub struct WorkerPackageStore {
    root: PathBuf,
    trust_root: PathBuf,
}

impl WorkerPackageStore {
    pub fn open(runtimes_root: &Path) -> Result<Self, ModelError> {
        let root = runtimes_root.join("model-workers");
        fs::create_dir_all(&root)?;
        let trust_root = runtimes_root
            .parent()
            .unwrap_or(runtimes_root)
            .to_path_buf();
        Ok(Self { root, trust_root })
    }

    pub fn download_install(
        &self,
        request: &WorkerPackageRequest,
    ) -> Result<InstalledWorkerPackage, ModelError> {
        validate_request(request)?;
        crate::core::trust::TrustStore::open(&self.trust_root)
            .map_err(|error| ModelError::InvalidMetadata(error.to_string()))?
            .require(&request.manifest.publisher_key)
            .map_err(|error| ModelError::InvalidMetadata(error.to_string()))?;
        let partial = self.root.join(".downloads").join(format!(
            "{}-{}.zip.part",
            request.manifest.worker_kind, request.manifest.version
        ));
        fs::create_dir_all(partial.parent().expect("download parent"))?;
        let agent = ureq::Agent::config_builder()
            .https_only(true)
            .timeout_global(Some(std::time::Duration::from_secs(1800)))
            .build()
            .into();
        crate::core::update::resumable_download(
            &agent,
            &request.manifest.url,
            &partial,
            request.manifest.size_bytes,
            &request.manifest.sha256,
            MAX_BYTES,
            &mut |_, _| true,
        )
        .map_err(|error| ModelError::Worker(error.to_string()))?;
        let installed = self.install_archive(request, &partial)?;
        let _ = fs::remove_file(partial);
        Ok(installed)
    }

    pub fn install_archive(
        &self,
        request: &WorkerPackageRequest,
        archive: &Path,
    ) -> Result<InstalledWorkerPackage, ModelError> {
        validate_request(request)?;
        let actual = file_digest(archive)?;
        if actual != request.manifest.sha256 {
            return Err(ModelError::DigestMismatch {
                expected: request.manifest.sha256.clone(),
                actual,
            });
        }
        if fs::metadata(archive)?.len() != request.manifest.size_bytes {
            return Err(ModelError::InvalidMetadata(
                "worker package size mismatch".into(),
            ));
        }
        let kind_root = self.root.join(&request.manifest.worker_kind);
        let destination = kind_root
            .join("versions")
            .join(&request.manifest.version)
            .join(&request.manifest.triple);
        if !destination.exists() {
            fs::create_dir_all(destination.parent().expect("version parent"))?;
            let temporary = tempfile::Builder::new()
                .prefix(".worker-extract-")
                .tempdir_in(destination.parent().expect("version parent"))?;
            extract_archive(archive, temporary.path())?;
            let descriptor_path = temporary.path().join("worker.json");
            let descriptor: WorkerDescriptor = serde_json::from_slice(&fs::read(&descriptor_path)?)
                .map_err(|error| ModelError::InvalidMetadata(error.to_string()))?;
            super::validate_worker_descriptor(&descriptor, &kind_root)?;
            let command = temporary.path().join(&descriptor.command).canonicalize()?;
            if !command.starts_with(temporary.path().canonicalize()?) || !command.is_file() {
                return Err(ModelError::InvalidMetadata(
                    "worker package executable is missing or escapes package".into(),
                ));
            }
            fs::write(
                temporary.path().join("package.json"),
                serde_json::to_vec_pretty(&request.manifest)
                    .map_err(|error| ModelError::InvalidMetadata(error.to_string()))?,
            )?;
            fs::rename(temporary.keep(), &destination)?;
        }
        self.activate(
            &request.manifest.worker_kind,
            &request.manifest.version,
            &request.manifest.triple,
        )?;
        Ok(InstalledWorkerPackage {
            engine: request.manifest.engine,
            worker_kind: request.manifest.worker_kind.clone(),
            version: request.manifest.version.clone(),
            triple: request.manifest.triple.clone(),
            root: destination,
            active: true,
        })
    }

    pub fn activate(&self, kind: &str, version: &str, triple: &str) -> Result<(), ModelError> {
        let target = self
            .root
            .join(kind)
            .join("versions")
            .join(version)
            .join(triple);
        if !target.join("worker.json").is_file() {
            return Err(ModelError::NotFound(format!("{kind}@{version}/{triple}")));
        }
        super::atomic_json(
            &self.root.join(kind).join("active.json"),
            &ActiveVersion {
                version: version.into(),
                triple: triple.into(),
            },
        )
    }

    pub fn deactivate(&self, kind: &str) -> Result<(), ModelError> {
        let path = self.root.join(kind).join("active.json");
        if path.exists() {
            fs::remove_file(path)?;
        }
        Ok(())
    }

    pub fn active_root(&self, kind_root: &Path) -> Result<Option<PathBuf>, ModelError> {
        let path = kind_root.join("active.json");
        if !path.is_file() {
            return Ok(None);
        }
        let active: ActiveVersion = serde_json::from_slice(&fs::read(path)?)
            .map_err(|error| ModelError::InvalidMetadata(error.to_string()))?;
        let root = kind_root
            .join("versions")
            .join(active.version)
            .join(active.triple);
        if !root.join("worker.json").is_file() {
            return Err(ModelError::InvalidMetadata(
                "active worker version is missing".into(),
            ));
        }
        Ok(Some(root))
    }

    pub fn runtimes_root(&self) -> PathBuf {
        self.root
            .parent()
            .expect("model-workers parent")
            .to_path_buf()
    }

    pub fn list(&self) -> Result<Vec<InstalledWorkerPackage>, ModelError> {
        let mut packages = Vec::new();
        for kind in fs::read_dir(&self.root)?
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|value| value.is_dir()))
        {
            let kind_root = kind.path();
            let active = self
                .active_root(&kind_root)?
                .and_then(|root| root.canonicalize().ok());
            let Ok(versions) = fs::read_dir(kind_root.join("versions")) else {
                continue;
            };
            for version in versions.flatten() {
                let Ok(triples) = fs::read_dir(version.path()) else {
                    continue;
                };
                for triple in triples.flatten() {
                    let root = triple.path();
                    let metadata = root.join("package.json");
                    if !metadata.is_file() {
                        continue;
                    }
                    let manifest: WorkerPackageManifest =
                        serde_json::from_slice(&fs::read(metadata)?)
                            .map_err(|error| ModelError::InvalidMetadata(error.to_string()))?;
                    let is_active = active
                        .as_ref()
                        .is_some_and(|value| root.canonicalize().ok().as_ref() == Some(value));
                    packages.push(InstalledWorkerPackage {
                        engine: manifest.engine,
                        worker_kind: manifest.worker_kind,
                        version: manifest.version,
                        triple: manifest.triple,
                        root,
                        active: is_active,
                    });
                }
            }
        }
        packages.sort_by(|a, b| {
            a.worker_kind.cmp(&b.worker_kind).then_with(|| {
                Version::parse(&b.version)
                    .ok()
                    .cmp(&Version::parse(&a.version).ok())
            })
        });
        Ok(packages)
    }
}

fn validate_request(request: &WorkerPackageRequest) -> Result<(), ModelError> {
    let manifest = &request.manifest;
    let expected_kind = match manifest.engine {
        WorkerEngine::LlamaCpp => "llama-cpp",
        WorkerEngine::OnnxRuntimeGenai => "onnxruntime-genai",
    };
    if manifest.worker_kind != expected_kind {
        return Err(ModelError::InvalidMetadata(
            "worker kind does not match its engine adapter".into(),
        ));
    }
    if manifest.schema_version != 1
        || Version::parse(&manifest.version).is_err()
        || manifest.triple != crate::runtime_provider::TargetTriple::host().dir_name()
    {
        return Err(ModelError::InvalidMetadata(
            "invalid worker package version or target".into(),
        ));
    }
    if !manifest.url.starts_with("https://")
        || manifest.sha256.len() != 64
        || !manifest.sha256.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(ModelError::InvalidMetadata(
            "worker package requires HTTPS and a SHA-256 digest".into(),
        ));
    }
    let key: [u8; 32] = base64::engine::general_purpose::STANDARD
        .decode(&manifest.publisher_key)
        .map_err(|_| ModelError::InvalidMetadata("invalid worker publisher key".into()))?
        .try_into()
        .map_err(|_| ModelError::InvalidMetadata("invalid worker publisher key length".into()))?;
    let signature: [u8; 64] = base64::engine::general_purpose::STANDARD
        .decode(&request.signature)
        .map_err(|_| ModelError::InvalidMetadata("invalid worker package signature".into()))?
        .try_into()
        .map_err(|_| ModelError::InvalidMetadata("invalid worker signature length".into()))?;
    let payload = serde_json::to_vec(manifest)
        .map_err(|error| ModelError::InvalidMetadata(error.to_string()))?;
    VerifyingKey::from_bytes(&key)
        .map_err(|error| ModelError::InvalidMetadata(error.to_string()))?
        .verify(&payload, &Signature::from_bytes(&signature))
        .map_err(|_| {
            ModelError::InvalidMetadata("worker package signature verification failed".into())
        })
}

fn extract_archive(archive: &Path, destination: &Path) -> Result<(), ModelError> {
    let mut zip = ZipArchive::new(fs::File::open(archive)?)
        .map_err(|error| ModelError::InvalidMetadata(error.to_string()))?;
    if zip.len() > MAX_FILES {
        return Err(ModelError::InvalidMetadata(
            "worker archive has too many files".into(),
        ));
    }
    let mut total = 0u64;
    for index in 0..zip.len() {
        let mut entry = zip
            .by_index(index)
            .map_err(|error| ModelError::InvalidMetadata(error.to_string()))?;
        let relative = entry
            .enclosed_name()
            .ok_or_else(|| ModelError::InvalidMetadata("worker archive path traversal".into()))?
            .to_owned();
        total = total.saturating_add(entry.size());
        if total > MAX_BYTES {
            return Err(ModelError::InvalidMetadata(
                "worker archive is too large".into(),
            ));
        }
        let output = destination.join(relative);
        if entry.is_dir() {
            fs::create_dir_all(output)?;
            continue;
        }
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = fs::File::create(output)?;
        std::io::copy(&mut entry, &mut file)?;
        file.flush()?;
    }
    Ok(())
}

fn file_digest(path: &Path) -> Result<String, ModelError> {
    let mut file = fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 1024 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn archive() -> Vec<u8> {
        let descriptor = WorkerDescriptor {
            schema_version: 1,
            kind: "llama-cpp".into(),
            command: "worker.exe".into(),
            args: vec![],
            providers: vec![super::super::hardware::ComputeProvider::Cpu],
            max_concurrency: 1,
            memory_overhead_mb: 64,
        };
        let mut bytes = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut bytes));
            let options = zip::write::SimpleFileOptions::default();
            zip.start_file("worker.json", options).unwrap();
            zip.write_all(&serde_json::to_vec(&descriptor).unwrap())
                .unwrap();
            zip.start_file("worker.exe", options).unwrap();
            zip.write_all(b"worker-binary").unwrap();
            zip.finish().unwrap();
        }
        bytes
    }

    fn request(bytes: &[u8], signing: &SigningKey) -> WorkerPackageRequest {
        let manifest = WorkerPackageManifest {
            schema_version: 1,
            engine: WorkerEngine::LlamaCpp,
            worker_kind: "llama-cpp".into(),
            version: "1.2.3".into(),
            triple: crate::runtime_provider::TargetTriple::host().dir_name(),
            url: "https://runtime.example/llama.zip".into(),
            sha256: format!("{:x}", Sha256::digest(bytes)),
            size_bytes: bytes.len() as u64,
            publisher_key: base64::engine::general_purpose::STANDARD
                .encode(signing.verifying_key().to_bytes()),
        };
        let signature = base64::engine::general_purpose::STANDARD.encode(
            signing
                .sign(&serde_json::to_vec(&manifest).unwrap())
                .to_bytes(),
        );
        WorkerPackageRequest {
            manifest,
            signature,
        }
    }

    #[test]
    fn signed_worker_package_installs_and_activates_atomically() {
        let temp = tempfile::tempdir().unwrap();
        let bytes = archive();
        let archive = temp.path().join("worker.zip");
        fs::write(&archive, &bytes).unwrap();
        let signing = SigningKey::from_bytes(&[9u8; 32]);
        let request = request(&bytes, &signing);
        let store = WorkerPackageStore::open(&temp.path().join("runtimes")).unwrap();
        let installed = store.install_archive(&request, &archive).unwrap();
        assert!(installed.active);
        let listed = store.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert!(listed[0].active);
        assert_eq!(
            store
                .active_root(&temp.path().join("runtimes/model-workers/llama-cpp"))
                .unwrap()
                .unwrap(),
            installed.root
        );
    }

    #[test]
    fn modified_worker_manifest_signature_is_rejected() {
        let bytes = archive();
        let signing = SigningKey::from_bytes(&[3u8; 32]);
        let mut request = request(&bytes, &signing);
        request.manifest.version = "1.2.4".into();
        assert!(
            validate_request(&request)
                .unwrap_err()
                .to_string()
                .contains("signature")
        );
    }
}
