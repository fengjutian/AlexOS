use std::{
    fs::OpenOptions,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    AlexError,
    package::{self, PackageError, UpdateResult},
    trust::{TrustError, TrustStore},
};

const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_DOWNLOAD_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateChannel {
    Stable,
    Beta,
    Dev,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateManifest {
    pub schema_version: u32,
    pub app_id: String,
    pub channel: UpdateChannel,
    pub version: String,
    pub url: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SignedUpdateManifest {
    pub manifest: UpdateManifest,
    pub public_key: String,
    pub signature: String,
}

#[derive(Debug, Error)]
pub enum UpdateError {
    #[error("update manifest is invalid: {0}")]
    Manifest(String),
    #[error("update transport failed: {0}")]
    Transport(String),
    #[error("update signature failed: {0}")]
    Signature(String),
    #[error(transparent)]
    Package(#[from] PackageError),
    #[error(transparent)]
    Trust(#[from] TrustError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Alex(#[from] AlexError),
}

pub fn manifest_for_package(
    app_id: String,
    channel: UpdateChannel,
    version: String,
    url: String,
    package_path: &Path,
) -> Result<UpdateManifest, UpdateError> {
    let (package_id, package_version) = package::archive_identity(package_path)?;
    if package_id != app_id || package_version != version {
        return Err(UpdateError::Manifest(format!(
            "package identity {package_id}@{package_version} does not match {app_id}@{version}"
        )));
    }
    let mut input = std::fs::File::open(package_path)?;
    let mut hasher = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        size += count as u64;
        hasher.update(&buffer[..count]);
    }
    let manifest = UpdateManifest {
        schema_version: 1,
        app_id,
        channel,
        version,
        url,
        sha256: format!("{:x}", hasher.finalize()),
        size,
    };
    validate_manifest(&manifest)?;
    Ok(manifest)
}

pub fn create_signed_manifest(
    manifest: UpdateManifest,
    key_path: &Path,
) -> Result<SignedUpdateManifest, UpdateError> {
    validate_manifest(&manifest)?;
    let payload =
        serde_json::to_vec(&manifest).map_err(|error| UpdateError::Manifest(error.to_string()))?;
    let (public_key, signature) = package::sign_payload(key_path, &payload)?;
    Ok(SignedUpdateManifest {
        manifest,
        public_key,
        signature,
    })
}

pub fn verify_manifest(
    envelope: &SignedUpdateManifest,
    app_id: &str,
    current_version: &str,
    channel: UpdateChannel,
    trust: &TrustStore,
) -> Result<(), UpdateError> {
    validate_manifest(&envelope.manifest)?;
    if envelope.manifest.app_id != app_id || envelope.manifest.channel != channel {
        return Err(UpdateError::Manifest(
            "application id or channel mismatch".into(),
        ));
    }
    let current = Version::parse(current_version)
        .map_err(|error| UpdateError::Manifest(error.to_string()))?;
    let offered = Version::parse(&envelope.manifest.version)
        .map_err(|error| UpdateError::Manifest(error.to_string()))?;
    if offered <= current {
        return Err(UpdateError::Manifest(format!(
            "{} is not newer than {current_version}",
            envelope.manifest.version
        )));
    }
    trust.require(&envelope.public_key)?;
    let public: [u8; 32] = BASE64
        .decode(&envelope.public_key)
        .map_err(|error| UpdateError::Signature(error.to_string()))?
        .try_into()
        .map_err(|_| UpdateError::Signature("invalid public key length".into()))?;
    let signature: [u8; 64] = BASE64
        .decode(&envelope.signature)
        .map_err(|error| UpdateError::Signature(error.to_string()))?
        .try_into()
        .map_err(|_| UpdateError::Signature("invalid signature length".into()))?;
    let payload = serde_json::to_vec(&envelope.manifest)
        .map_err(|error| UpdateError::Manifest(error.to_string()))?;
    VerifyingKey::from_bytes(&public)
        .map_err(|error| UpdateError::Signature(error.to_string()))?
        .verify(&payload, &Signature::from_bytes(&signature))
        .map_err(|error| UpdateError::Signature(error.to_string()))
}

pub fn update_from_url(
    manifest_url: &str,
    install_root: &Path,
    app_id: &str,
    channel: UpdateChannel,
    trust_root: &Path,
) -> Result<UpdateResult, UpdateError> {
    update_from_url_with_progress(
        manifest_url,
        install_root,
        app_id,
        channel,
        trust_root,
        |_, _| true,
    )
}

pub fn update_from_url_with_progress(
    manifest_url: &str,
    install_root: &Path,
    app_id: &str,
    channel: UpdateChannel,
    trust_root: &Path,
    mut progress: impl FnMut(&str, u8) -> bool,
) -> Result<UpdateResult, UpdateError> {
    if !progress("checking", 5) {
        return Err(UpdateError::Transport("update cancelled".into()));
    }
    require_https(manifest_url)?;
    let current = crate::load_app(&install_root.join(app_id))?;
    let agent = secure_agent();
    let mut response = agent
        .get(manifest_url)
        .call()
        .map_err(|error| UpdateError::Transport(error.to_string()))?;
    let bytes = response
        .body_mut()
        .with_config()
        .limit(MAX_MANIFEST_BYTES)
        .read_to_vec()
        .map_err(|error| UpdateError::Transport(error.to_string()))?;
    let envelope: SignedUpdateManifest =
        serde_json::from_slice(&bytes).map_err(|error| UpdateError::Manifest(error.to_string()))?;
    let trust = TrustStore::open(trust_root)?;
    verify_manifest(&envelope, app_id, &current.version, channel, &trust)?;
    if !progress("verified", 15) {
        return Err(UpdateError::Transport("update cancelled".into()));
    }

    let package_file = download_package(&agent, &envelope.manifest, install_root, &mut progress)?;
    if !progress("installing", 90) {
        return Err(UpdateError::Transport("update cancelled".into()));
    }
    package::update_verified(
        &package_file,
        install_root,
        true,
        Some(&envelope.public_key),
        false,
    )
    .map(|result| {
        let _ = std::fs::remove_file(&package_file);
        let _ = std::fs::remove_file(resume_meta_path(&package_file));
        result
    })
    .map_err(Into::into)
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResumeMetadata {
    url: String,
    etag: Option<String>,
    expected_size: u64,
    sha256: String,
}

fn download_package(
    agent: &ureq::Agent,
    manifest: &UpdateManifest,
    install_root: &Path,
    progress: &mut impl FnMut(&str, u8) -> bool,
) -> Result<PathBuf, UpdateError> {
    require_https(&manifest.url)?;
    let download_dir = install_root.join(".alex").join("downloads");
    std::fs::create_dir_all(&download_dir)?;
    let partial = download_dir.join(format!(
        "{}-{}.alex.part",
        manifest.app_id, manifest.version
    ));
    resumable_download(
        agent,
        &manifest.url,
        &partial,
        manifest.size,
        &manifest.sha256,
        MAX_DOWNLOAD_BYTES,
        &mut |downloaded, total| {
            let percent = 15 + ((downloaded.saturating_mul(70) / total.max(1)).min(70) as u8);
            progress("downloading", percent)
        },
    )
}

pub(crate) fn resumable_download(
    agent: &ureq::Agent,
    url: &str,
    partial: &Path,
    expected_size: u64,
    sha256: &str,
    max_bytes: u64,
    progress: &mut impl FnMut(u64, u64) -> bool,
) -> Result<PathBuf, UpdateError> {
    require_https(url)?;
    let metadata_path = resume_meta_path(&partial);
    let metadata = std::fs::read(&metadata_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<ResumeMetadata>(&bytes).ok());
    let mut offset = std::fs::metadata(&partial).map(|m| m.len()).unwrap_or(0);
    if (offset > 0 && metadata.is_none())
        || offset > expected_size
        || metadata.as_ref().is_some_and(|m| {
            m.url != url || m.expected_size != expected_size || m.sha256 != sha256
        })
    {
        let _ = std::fs::remove_file(&partial);
        let _ = std::fs::remove_file(&metadata_path);
        offset = 0;
    }
    if offset == expected_size && file_sha256(&partial)? == sha256 {
        return Ok(partial.to_path_buf());
    }
    let mut request = agent.get(url);
    if offset > 0 {
        request = request.header("Range", format!("bytes={offset}-"));
        if let Some(etag) = metadata.as_ref().and_then(|m| m.etag.as_deref()) {
            request = request.header("If-Range", etag);
        }
    }
    let mut response = request
        .call()
        .map_err(|error| UpdateError::Transport(error.to_string()))?;
    let partial_response = response.status().as_u16() == 206;
    if offset > 0 && !partial_response {
        offset = 0;
    }
    if partial_response {
        let expected_prefix = format!("bytes {offset}-");
        let valid_range = response
            .headers()
            .get("content-range")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with(&expected_prefix));
        if !valid_range {
            return Err(UpdateError::Transport(
                "server returned an invalid Content-Range".into(),
            ));
        }
    }
    let etag = response
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .or_else(|| metadata.as_ref().and_then(|m| m.etag.clone()));
    std::fs::write(
        &metadata_path,
        serde_json::to_vec_pretty(&ResumeMetadata {
            url: url.to_owned(),
            etag,
            expected_size,
            sha256: sha256.to_owned(),
        })
        .map_err(|error| UpdateError::Manifest(error.to_string()))?,
    )?;
    let mut reader = response
        .body_mut()
        .with_config()
        .limit(max_bytes + 1)
        .reader();
    let mut output = OpenOptions::new()
        .create(true)
        .write(true)
        .append(offset > 0)
        .truncate(offset == 0)
        .open(&partial)?;
    let mut hasher = Sha256::new();
    if offset > 0 {
        let mut existing = std::fs::File::open(&partial)?;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let count = existing.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hasher.update(&buffer[..count]);
        }
    }
    let mut size = offset;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = reader
            .read(&mut buffer)
            .map_err(|error| UpdateError::Transport(error.to_string()))?;
        if count == 0 {
            break;
        }
        size += count as u64;
        if size > max_bytes {
            return Err(UpdateError::Transport(format!("download exceeds {max_bytes} bytes")));
        }
        output.write_all(&buffer[..count])?;
        hasher.update(&buffer[..count]);
        if !progress(size, expected_size) {
            return Err(UpdateError::Transport("download cancelled".into()));
        }
    }
    if size != expected_size || format!("{:x}", hasher.finalize()) != sha256 {
        return Err(UpdateError::Manifest(
            "download size or SHA-256 mismatch".into(),
        ));
    }
    output.flush()?;
    Ok(partial.to_path_buf())
}

fn resume_meta_path(partial: &Path) -> PathBuf {
    partial.with_extension("part.json")
}

fn file_sha256(path: &Path) -> Result<String, UpdateError> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn validate_manifest(manifest: &UpdateManifest) -> Result<(), UpdateError> {
    if manifest.schema_version != 1 || manifest.size == 0 || manifest.size > MAX_DOWNLOAD_BYTES {
        return Err(UpdateError::Manifest(
            "unsupported schema or package size".into(),
        ));
    }
    require_https(&manifest.url)?;
    if manifest.sha256.len() != 64 || !manifest.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(UpdateError::Manifest("invalid SHA-256".into()));
    }
    Version::parse(&manifest.version).map_err(|error| UpdateError::Manifest(error.to_string()))?;
    Ok(())
}

fn require_https(value: &str) -> Result<(), UpdateError> {
    let url = url::Url::parse(value).map_err(|error| UpdateError::Manifest(error.to_string()))?;
    if url.scheme() != "https" {
        return Err(UpdateError::Manifest("update URLs must use HTTPS".into()));
    }
    Ok(())
}

fn secure_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .https_only(true)
        .max_redirects(3)
        .timeout_global(Some(Duration::from_secs(30)))
        .build()
        .into()
}
