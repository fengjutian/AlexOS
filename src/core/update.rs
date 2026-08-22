use std::{
    io::{Read, Write},
    path::Path,
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

    let package_file = download_package(&agent, &envelope.manifest, install_root)?;
    package::update_verified(
        package_file.path(),
        install_root,
        true,
        Some(&envelope.public_key),
        false,
    )
    .map_err(Into::into)
}

fn download_package(
    agent: &ureq::Agent,
    manifest: &UpdateManifest,
    install_root: &Path,
) -> Result<tempfile::NamedTempFile, UpdateError> {
    require_https(&manifest.url)?;
    let mut response = agent
        .get(&manifest.url)
        .call()
        .map_err(|error| UpdateError::Transport(error.to_string()))?;
    let mut reader = response
        .body_mut()
        .with_config()
        .limit(MAX_DOWNLOAD_BYTES + 1)
        .reader();
    let mut output = tempfile::Builder::new()
        .suffix(".alex")
        .tempfile_in(install_root)?;
    let mut hasher = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = reader
            .read(&mut buffer)
            .map_err(|error| UpdateError::Transport(error.to_string()))?;
        if count == 0 {
            break;
        }
        size += count as u64;
        if size > MAX_DOWNLOAD_BYTES {
            return Err(UpdateError::Transport("download exceeds 512 MiB".into()));
        }
        output.write_all(&buffer[..count])?;
        hasher.update(&buffer[..count]);
    }
    if size != manifest.size || format!("{:x}", hasher.finalize()) != manifest.sha256 {
        return Err(UpdateError::Manifest(
            "download size or SHA-256 mismatch".into(),
        ));
    }
    Ok(output)
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
