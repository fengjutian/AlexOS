//! Managed runtime resolution (roadmap 0.3 — 受管 Runtime).
//!
//! The 0.1 host discovers Node.js through `ALEX_NODE` or the ambient
//! `PATH`, which makes an app's behaviour depend on whatever the user
//! happened to install. This module introduces a *managed* runtime
//! store: runtimes are versioned, downloaded over HTTPS, verified
//! against a SHA-256 digest, cached under a per-target-triple layout,
//! and evicted on an LRU basis. A system runtime remains a fallback so
//! the existing behaviour is preserved until an operator opts an app
//! into `requireManaged`.
//!
//! ```text
//! <cache_root>/
//!   node/22.14.0/windows-x86_64/   node.exe + .alex-runtime.json
//!   python/3.12.4/windows-x86_64/   python.exe + .alex-runtime.json
//! ```
//!
//! This module is self-contained and testable without a network or a
//! real Node install; the download/verify/extract/evict machinery is
//! exercised against an in-memory downloader and a synthetic archive.

use std::{
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use zip::ZipArchive;

use crate::core::manifest_v2::ServiceRuntime;

/// Subdirectory of the Alex data root that holds managed runtimes.
pub const RUNTIME_CACHE_DIR: &str = "runtimes";

/// Hard caps applied while extracting a downloaded runtime archive, so
/// a malicious or corrupt package cannot exhaust the disk.
const MAX_ARCHIVE_FILES: usize = 100_000;
const MAX_ARCHIVE_BYTES: u64 = 2 * 1024 * 1024 * 1024; // 2 GiB
const MAX_SINGLE_FILE_BYTES: u64 = 1024 * 1024 * 1024; // 1 GiB

// ---------------------------------------------------------------------
// Target triple
// ---------------------------------------------------------------------

/// The OS/architecture slice a runtime package targets. Cached runtimes
/// are keyed by this so a Node built for `windows-x86_64` is never
/// handed to a `linux-aarch64` host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetTriple {
    pub os: String,
    pub arch: String,
}

impl TargetTriple {
    /// The triple of the host this process is running on.
    pub fn host() -> Self {
        Self {
            os: normalize_os(std::env::consts::OS).to_owned(),
            arch: normalize_arch(std::env::consts::ARCH).to_owned(),
        }
    }

    /// The cache directory name derived from this triple.
    pub fn dir_name(&self) -> String {
        format!("{}-{}", self.os, self.arch)
    }
}

fn normalize_os(os: &str) -> &str {
    match os {
        "windows" => "windows",
        "macos" => "macos",
        "linux" => "linux",
        other => other,
    }
}

fn normalize_arch(arch: &str) -> &str {
    match arch {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        "x86" => "x86",
        other => other,
    }
}

// ---------------------------------------------------------------------
// Model types
// ---------------------------------------------------------------------

/// How a resolved runtime was satisfied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeSource {
    /// Resolved from the managed, versioned cache.
    Managed,
    /// Fell back to a system runtime (`ALEX_NODE` / `PATH`).
    System,
}

/// A concrete runtime executable ready to be spawned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRuntime {
    pub executable: PathBuf,
    pub version: Option<String>,
    pub source: RuntimeSource,
}

/// A runtime the host is asked to provide for a service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeRequest {
    pub kind: ServiceRuntime,
    /// Optional version requirement (e.g. `"22"`, `"22.4"`, `"^3.12"`).
    pub version_req: Option<String>,
    pub triple: TargetTriple,
    /// When `true`, a system runtime is never an acceptable fallback —
    /// the app must be satisfied from the managed cache (the "apps do
    /// not depend on the user's PATH" guarantee from the roadmap).
    pub require_managed: bool,
}

/// A runtime installed in the managed cache.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledRuntime {
    pub kind: ServiceRuntime,
    pub version: String,
    pub triple: String,
    pub root: PathBuf,
}

/// A catalog entry describing a downloadable runtime package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimePackage {
    pub version: String,
    pub url: String,
    pub sha256: String,
}

/// On-disk metadata recorded beside an installed runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeManifest {
    kind: ServiceRuntime,
    version: String,
    triple: String,
    sha256: String,
}

const MANIFEST_FILE: &str = ".alex-runtime.json";

#[derive(Debug, Error)]
pub enum RuntimeProviderError {
    #[error("no runtime satisfies {kind:?} {req:?}")]
    NotAvailable {
        kind: ServiceRuntime,
        req: Option<String>,
    },
    #[error("invalid runtime version requirement {0}")]
    InvalidVersionReq(String),
    #[error("runtime download failed: {0}")]
    Download(String),
    #[error("runtime archive verification failed: {0}")]
    Verify(String),
    #[error("runtime archive is unsafe: {0}")]
    UnsafeArchive(String),
    #[error("runtime I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

// ---------------------------------------------------------------------
// Downloader
// ---------------------------------------------------------------------

/// Downloads a runtime package to `dest`. Kept behind a trait so the
/// provider is testable without a network.
pub trait Downloader: Send + Sync {
    fn fetch(&self, url: &str, dest: &Path) -> Result<(), String>;
}

/// HTTPS-only downloader backed by `ureq`.
pub struct HttpDownloader {
    agent: ureq::Agent,
}

impl HttpDownloader {
    pub fn new() -> Self {
        let agent = ureq::Agent::config_builder()
            .https_only(true)
            .max_redirects(3)
            .timeout_global(Some(Duration::from_secs(300)))
            .build()
            .into();
        Self { agent }
    }
}

impl Default for HttpDownloader {
    fn default() -> Self {
        Self::new()
    }
}

impl Downloader for HttpDownloader {
    fn fetch(&self, url: &str, dest: &Path) -> Result<(), String> {
        let mut response = self
            .agent
            .get(url)
            .call()
            .map_err(|error| error.to_string())?;
        let bytes = response
            .body_mut()
            .with_config()
            .limit(MAX_ARCHIVE_BYTES)
            .read_to_vec()
            .map_err(|error| error.to_string())?;
        fs::write(dest, bytes).map_err(|error| error.to_string())
    }
}

// ---------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------

/// Resolves runtimes against the managed cache, falling back to a
/// system runtime when permitted. All methods are safe to call from
/// multiple threads.
pub struct RuntimeProvider {
    cache_root: PathBuf,
    system_node: Arc<dyn Fn() -> Option<PathBuf> + Send + Sync>,
    downloader: Arc<dyn Downloader>,
    max_versions: usize,
}

impl RuntimeProvider {
    /// The production provider: managed cache under the Alex data root,
    /// a caller-supplied system-Node resolver (the host's
    /// `ALEX_NODE` / `PATH` lookup), and real HTTPS downloads. The
    /// resolver is injected rather than referenced directly so this
    /// module stays independent of the runtime supervisor.
    pub fn system(system_node: Arc<dyn Fn() -> Option<PathBuf> + Send + Sync>) -> Self {
        Self {
            cache_root: default_cache_root(),
            system_node,
            downloader: Arc::new(HttpDownloader::new()),
            max_versions: 4,
        }
    }

    /// A provider rooted at `cache_root` with no system fallback and a
    /// failing downloader. Primarily for tests and `--offline` use.
    pub fn with_root(cache_root: PathBuf, downloader: Arc<dyn Downloader>) -> Self {
        Self {
            cache_root,
            system_node: Arc::new(|| None),
            downloader,
            max_versions: 4,
        }
    }

    /// Resolve a request. Managed cache is consulted first; a system
    /// Node fallback is used only when `require_managed` is `false`.
    pub fn resolve(
        &self,
        request: &RuntimeRequest,
    ) -> Result<ResolvedRuntime, RuntimeProviderError> {
        if let Some(version) = self.find_matching(request)? {
            let root = self.version_dir(request.kind, &version, &request.triple);
            let executable = executable_for(request.kind, &root).ok_or_else(|| {
                RuntimeProviderError::Verify(format!(
                    "cached runtime at {} has no executable",
                    root.display()
                ))
            })?;
            return Ok(ResolvedRuntime {
                executable,
                version: Some(version),
                source: RuntimeSource::Managed,
            });
        }
        if !request.require_managed {
            if let Some(executable) = self.system_fallback(request.kind) {
                return Ok(ResolvedRuntime {
                    executable,
                    version: None,
                    source: RuntimeSource::System,
                });
            }
        }
        Err(RuntimeProviderError::NotAvailable {
            kind: request.kind,
            req: request.version_req.clone(),
        })
    }

    /// Enumerate the managed runtimes installed for `kind`, newest
    /// first.
    pub fn installed(&self, kind: ServiceRuntime) -> Vec<InstalledRuntime> {
        let kind_dir = self.cache_root.join(kind_dir_name(kind));
        let mut out = Vec::new();
        let Ok(versions) = fs::read_dir(&kind_dir) else {
            return out;
        };
        for version in versions.flatten() {
            let version_path = version.path();
            if !version_path.is_dir() {
                continue;
            }
            let version_name = version_path.file_name().unwrap_or_default().to_string_lossy();
            let Ok(triples) = fs::read_dir(&version_path) else {
                continue;
            };
            for triple in triples.flatten() {
                let root = triple.path();
                if root.is_dir() && root.join(MANIFEST_FILE).is_file() {
                    out.push(InstalledRuntime {
                        kind,
                        version: version_name.to_string(),
                        triple: triple.file_name().to_string_lossy().into_owned(),
                        root,
                    });
                }
            }
        }
        out.sort_by(|a, b| b.version.cmp(&a.version));
        out
    }

    /// Download, verify and install a runtime package, then return the
    /// resolved executable. Idempotent: an already-installed version is
    /// returned without re-downloading.
    pub fn install(
        &self,
        kind: ServiceRuntime,
        triple: &TargetTriple,
        package: &RuntimePackage,
    ) -> Result<ResolvedRuntime, RuntimeProviderError> {
        let version = Version::parse(&package.version)
            .map_err(|error| RuntimeProviderError::InvalidVersionReq(error.to_string()))?;
        let dest = self.version_dir(kind, &package.version, triple);
        if dest.join(MANIFEST_FILE).is_file() {
            let executable = executable_for(kind, &dest).ok_or_else(|| {
                RuntimeProviderError::Verify(format!(
                    "cached runtime at {} has no executable",
                    dest.display()
                ))
            })?;
            return Ok(ResolvedRuntime {
                executable,
                version: Some(package.version.clone()),
                source: RuntimeSource::Managed,
            });
        }

        let archive = self.download_and_verify(package, triple)?;
        let extracted = self.extract(kind, &version, triple, &archive)?;
        let _ = fs::remove_file(&archive);

        // Atomically publish: extract to a sibling temp dir, then rename.
        let parent = dest
            .parent()
            .ok_or_else(|| RuntimeProviderError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "cache destination has no parent",
            )))?;
        fs::create_dir_all(parent)?;
        let temp = parent.join(format!(".installing-{}-{}", std::process::id(), version));
        if temp.exists() {
            fs::remove_dir_all(&temp)?;
        }
        fs::rename(&extracted, &temp)?;
        fs::write(
            temp.join(MANIFEST_FILE),
            serde_json::to_vec_pretty(&RuntimeManifest {
                kind,
                version: package.version.clone(),
                triple: triple.dir_name(),
                sha256: package.sha256.clone(),
            })
            .map_err(|error| RuntimeProviderError::Io(std::io::Error::other(error)))?,
        )?;
        if dest.exists() {
            fs::remove_dir_all(&dest)?;
        }
        fs::rename(&temp, &dest)?;

        self.evict(kind);

        let executable = executable_for(kind, &dest).ok_or_else(|| {
            RuntimeProviderError::Verify(format!(
                "installed runtime at {} has no executable",
                dest.display()
            ))
        })?;
        Ok(ResolvedRuntime {
            executable,
            version: Some(package.version.clone()),
            source: RuntimeSource::Managed,
        })
    }

    fn download_and_verify(
        &self,
        package: &RuntimePackage,
        triple: &TargetTriple,
    ) -> Result<PathBuf, RuntimeProviderError> {
        let temp_dir = self
            .cache_root
            .join(format!(".download-{}-{}", std::process::id(), package.version));
        fs::create_dir_all(&temp_dir)?;
        let archive = temp_dir.join(format!("{}-{}.zip", kind_dir_name_file(package), triple.dir_name()));
        self.downloader
            .fetch(&package.url, &archive)
            .map_err(RuntimeProviderError::Download)?;
        let actual = sha256_file(&archive)?;
        if !actual.eq_ignore_ascii_case(&package.sha256) {
            let _ = fs::remove_file(&archive);
            return Err(RuntimeProviderError::Verify(format!(
                "sha256 mismatch: expected {}, got {}",
                package.sha256, actual
            )));
        }
        Ok(archive)
    }

    fn extract(
        &self,
        kind: ServiceRuntime,
        version: &Version,
        triple: &TargetTriple,
        archive: &Path,
    ) -> Result<PathBuf, RuntimeProviderError> {
        let parent = self
            .version_dir(kind, &version.to_string(), triple)
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.cache_root.clone());
        let extracted = parent.join(format!(
            ".extract-{}-{}-{}-{}",
            std::process::id(),
            kind_dir_name(kind),
            version,
            triple.dir_name()
        ));
        if extracted.exists() {
            fs::remove_dir_all(&extracted)?;
        }
        fs::create_dir_all(&extracted)?;

        let mut zip = ZipArchive::new(File::open(archive)?)
            .map_err(|error| RuntimeProviderError::UnsafeArchive(error.to_string()))?;
        if zip.len() > MAX_ARCHIVE_FILES {
            return Err(RuntimeProviderError::UnsafeArchive(format!(
                "more than {MAX_ARCHIVE_FILES} files"
            )));
        }
        let mut total_bytes = 0_u64;
        for index in 0..zip.len() {
            let mut entry = zip
                .by_index(index)
                .map_err(|error| RuntimeProviderError::UnsafeArchive(error.to_string()))?;
            let relative = entry.enclosed_name().ok_or_else(|| {
                RuntimeProviderError::UnsafeArchive(entry.name().to_owned())
            })?;
            if entry.size() > MAX_SINGLE_FILE_BYTES {
                return Err(RuntimeProviderError::UnsafeArchive(format!(
                    "{} exceeds {MAX_SINGLE_FILE_BYTES} bytes",
                    entry.name()
                )));
            }
            total_bytes = total_bytes.saturating_add(entry.size());
            if total_bytes > MAX_ARCHIVE_BYTES {
                return Err(RuntimeProviderError::UnsafeArchive(format!(
                    "expanded content exceeds {MAX_ARCHIVE_BYTES} bytes"
                )));
            }
            let destination = extracted.join(&relative);
            if entry.is_dir() {
                fs::create_dir_all(&destination)?;
                continue;
            }
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut output = File::create(&destination)?;
            let mut copied = 0_u64;
            let mut buffer = [0_u8; 8192];
            loop {
                let n = entry
                    .read(&mut buffer)
                    .map_err(|error| RuntimeProviderError::UnsafeArchive(error.to_string()))?;
                if n == 0 {
                    break;
                }
                std::io::Write::write_all(&mut output, &buffer[..n])?;
                copied += n as u64;
            }
            if copied != entry.size() {
                return Err(RuntimeProviderError::Verify(format!(
                    "size changed while extracting {}",
                    entry.name()
                )));
            }
        }
        Ok(extracted)
    }

    /// Remove the oldest versions of `kind` beyond `max_versions`, so a
    /// long-lived install does not accumulate unbounded disk usage.
    /// Retention is by semver (newest wins), which is deterministic and
    /// therefore testable; note this can in principle evict a version a
    /// still-running app holds open — a future slice will add in-use
    /// ref-counting before evicting.
    pub fn evict(&self, kind: ServiceRuntime) {
        let kind_dir = self.cache_root.join(kind_dir_name(kind));
        let Ok(entries) = fs::read_dir(&kind_dir) else {
            return;
        };
        let mut versions: Vec<Version> = entries
            .flatten()
            .filter(|entry| entry.path().is_dir())
            .filter_map(|entry| Version::parse(&entry.file_name().to_string_lossy()).ok())
            .collect();
        // Oldest first.
        versions.sort();
        while versions.len() > self.max_versions {
            let version = versions.remove(0);
            let path = kind_dir.join(version.to_string());
            let _ = fs::remove_dir_all(path);
        }
    }

    fn version_dir(&self, kind: ServiceRuntime, version: &str, triple: &TargetTriple) -> PathBuf {
        self.cache_root
            .join(kind_dir_name(kind))
            .join(version)
            .join(triple.dir_name())
    }

    fn find_matching(&self, request: &RuntimeRequest) -> Result<Option<String>, RuntimeProviderError> {
        let kind_dir = self.cache_root.join(kind_dir_name(request.kind));
        let Ok(entries) = fs::read_dir(&kind_dir) else {
            return Ok(None);
        };
        let mut matches: Vec<Version> = Vec::new();
        for entry in entries.flatten() {
            if !entry.path().is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(version) = Version::parse(&name) else {
                continue;
            };
            if version_matches(&version, request.version_req.as_deref())? {
                matches.push(version);
            }
        }
        matches.sort();
        Ok(matches.pop().map(|version| version.to_string()))
    }

    fn system_fallback(&self, kind: ServiceRuntime) -> Option<PathBuf> {
        match kind {
            ServiceRuntime::Node => (self.system_node)(),
            // Python and Native have no ambient-PATH fallback: they must
            // come from the managed cache (or be refused).
            ServiceRuntime::Python | ServiceRuntime::Native => None,
        }
    }
}

fn kind_dir_name(kind: ServiceRuntime) -> &'static str {
    match kind {
        ServiceRuntime::Node => "node",
        ServiceRuntime::Python => "python",
        ServiceRuntime::Native => "native",
    }
}

fn kind_dir_name_file(package: &RuntimePackage) -> String {
    // The archive is named after its version only; the kind is implicit
    // in the directory it lands in.
    format!("runtime-{}", package.version)
}

/// Locate the executable inside an extracted runtime tree. Node and
/// Python distributions differ in layout (Windows ships the binary at
/// the archive root, Unix tarballs under `bin/`), so probe a small set
/// of candidates.
fn executable_for(kind: ServiceRuntime, root: &Path) -> Option<PathBuf> {
    let names: &[&str] = match kind {
        ServiceRuntime::Node => {
            if cfg!(windows) {
                &["node.exe", "bin/node.exe"]
            } else {
                &["bin/node", "node"]
            }
        }
        ServiceRuntime::Python => {
            if cfg!(windows) {
                &["python.exe", "python3.exe"]
            } else {
                &["bin/python3", "bin/python"]
            }
        }
        ServiceRuntime::Native => &[],
    };
    names.iter().map(|name| root.join(name)).find(|path| path.is_file())
}

fn version_matches(version: &Version, req: Option<&str>) -> Result<bool, RuntimeProviderError> {
    let Some(req) = req else {
        return Ok(true);
    };
    let req = parse_version_req(req)?;
    Ok(req.matches(version))
}

/// Parse a runtime version requirement using the standard `semver`
/// `VersionReq` syntax. A bare major (`"22"`) or major.minor
/// (`"22.5"`) is interpreted with caret semantics by the `semver`
/// crate — `"22"` matches any `22.x`, `"22.5"` matches `>=22.5.0,
/// <23.0.0`.
fn parse_version_req(input: &str) -> Result<VersionReq, RuntimeProviderError> {
    VersionReq::parse(input)
        .map_err(|error| RuntimeProviderError::InvalidVersionReq(error.to_string()))
}

fn sha256_file(path: &Path) -> Result<String, std::io::Error> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn default_cache_root() -> PathBuf {
    if let Some(root) = std::env::var_os("ALEX_DATA_DIR") {
        return PathBuf::from(root).join("AlexOS").join(RUNTIME_CACHE_DIR);
    }
    crate::container::volume::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("AlexOS")
        .join(RUNTIME_CACHE_DIR)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// In-memory downloader that serves pre-baked bytes.
    struct MemoryDownloader {
        bytes: BTreeMap<String, Vec<u8>>,
    }

    impl Downloader for MemoryDownloader {
        fn fetch(&self, url: &str, dest: &Path) -> Result<(), String> {
            let bytes = self.bytes.get(url).ok_or_else(|| "not found".to_owned())?;
            fs::write(dest, bytes).map_err(|error| error.to_string())
        }
    }

    /// Build a tiny zip containing a single `node.exe`-style file, so
    /// `install` can run end-to-end without a real distribution.
    fn node_zip() -> Vec<u8> {
        let mut buffer = std::io::Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut buffer);
            let options = zip::write::SimpleFileOptions::default();
            writer.start_file("node.exe", options).unwrap();
            std::io::Write::write_all(&mut writer, b"#!/fake node\n").unwrap();
            writer.finish().unwrap();
        }
        buffer.into_inner()
    }

    fn provider() -> (tempfile::TempDir, RuntimeProvider) {
        let temp = tempfile::tempdir().unwrap();
        let provider = RuntimeProvider::with_root(
            temp.path().join("runtimes"),
            Arc::new(MemoryDownloader {
                bytes: BTreeMap::new(),
            }),
        );
        (temp, provider)
    }

    fn node_request(version_req: Option<&str>, require_managed: bool) -> RuntimeRequest {
        RuntimeRequest {
            kind: ServiceRuntime::Node,
            version_req: version_req.map(str::to_owned),
            triple: TargetTriple::host(),
            require_managed,
        }
    }

    #[test]
    fn host_triple_is_a_non_empty_dir_name() {
        let triple = TargetTriple::host();
        assert!(!triple.os.is_empty());
        assert!(!triple.arch.is_empty());
        assert!(!triple.dir_name().is_empty());
    }

    #[test]
    fn version_req_shorthand_matches_expected_ranges() {
        let v22 = Version::parse("22.14.0").unwrap();
        let v21 = Version::parse("21.7.3").unwrap();
        let v22_5 = Version::parse("22.5.0").unwrap();
        let v22_6 = Version::parse("22.6.1").unwrap();

        assert!(version_matches(&v22, Some("22")).unwrap());
        assert!(!version_matches(&v21, Some("22")).unwrap());

        // "22.5" is caret: >=22.5.0, <23.0.0.
        assert!(version_matches(&v22_5, Some("22.5")).unwrap());
        assert!(version_matches(&v22_6, Some("22.5")).unwrap());
        assert!(version_matches(&v22, Some("22.5")).unwrap());

        // A full semver req still works.
        assert!(version_matches(&v22, Some(">=21, <23")).unwrap());
        assert!(!version_matches(&v21, Some(">=22")).unwrap());
    }

    #[test]
    fn resolve_prefers_newest_matching_managed_version() {
        let (temp, provider) = provider();
        let root = temp.path().join("runtimes");
        for version in ["22.1.0", "22.14.0", "21.7.3"] {
            let dir = root
                .join("node")
                .join(version)
                .join(TargetTriple::host().dir_name());
            fs::create_dir_all(&dir).unwrap();
            let exe_name = if cfg!(windows) { "node.exe" } else { "bin/node" };
            let exe = dir.join(exe_name);
            if let Some(parent) = exe.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&exe, "fake").unwrap();
        }
        let resolved = provider.resolve(&node_request(Some("22"), false)).unwrap();
        assert_eq!(resolved.source, RuntimeSource::Managed);
        assert_eq!(resolved.version.as_deref(), Some("22.14.0"));
    }

    #[test]
    fn resolve_falls_back_to_system_when_unmanaged() {
        // `with_root` installs a no-system provider, so build one with a
        // system fallback explicitly.
        let temp = tempfile::tempdir().unwrap();
        let system_node = temp.path().join("node.exe");
        fs::write(&system_node, "fake").unwrap();
        let provider = RuntimeProvider {
            cache_root: temp.path().join("runtimes"),
            system_node: Arc::new(move || Some(system_node.clone())),
            downloader: Arc::new(MemoryDownloader {
                bytes: BTreeMap::new(),
            }),
            max_versions: 4,
        };
        let resolved = provider.resolve(&node_request(None, false)).unwrap();
        assert_eq!(resolved.source, RuntimeSource::System);
        assert!(resolved.version.is_none());
    }

    #[test]
    fn require_managed_refuses_system_fallback() {
        let temp = tempfile::tempdir().unwrap();
        let system_node = temp.path().join("node.exe");
        fs::write(&system_node, "fake").unwrap();
        let provider = RuntimeProvider {
            cache_root: temp.path().join("runtimes"),
            system_node: Arc::new(move || Some(system_node.clone())),
            downloader: Arc::new(MemoryDownloader {
                bytes: BTreeMap::new(),
            }),
            max_versions: 4,
        };
        assert!(matches!(
            provider.resolve(&node_request(None, true)),
            Err(RuntimeProviderError::NotAvailable { .. })
        ));
    }

    #[test]
    fn install_downloads_verifies_and_resolves() {
        let (temp, _provider) = provider();
        let zip = node_zip();
        let digest = {
            let mut hasher = Sha256::new();
            hasher.update(&zip);
            hex(&hasher.finalize())
        };
        let provider = RuntimeProvider {
            cache_root: temp.path().join("runtimes"),
            system_node: Arc::new(|| None),
            downloader: Arc::new(MemoryDownloader {
                bytes: BTreeMap::from([("https://example.test/node.zip".to_owned(), zip)]),
            }),
            max_versions: 4,
        };
        let package = RuntimePackage {
            version: "22.14.0".to_owned(),
            url: "https://example.test/node.zip".to_owned(),
            sha256: digest,
        };
        let resolved = provider
            .install(ServiceRuntime::Node, &TargetTriple::host(), &package)
            .unwrap();
        assert_eq!(resolved.source, RuntimeSource::Managed);
        assert_eq!(resolved.version.as_deref(), Some("22.14.0"));

        // Second install is idempotent and hits the cache.
        let again = provider
            .install(ServiceRuntime::Node, &TargetTriple::host(), &package)
            .unwrap();
        assert_eq!(again.executable, resolved.executable);

        // And now `resolve` sees it as managed.
        let via_resolve = provider.resolve(&node_request(Some("22"), false)).unwrap();
        assert_eq!(via_resolve.source, RuntimeSource::Managed);
    }

    #[test]
    fn install_rejects_a_sha256_mismatch() {
        let (temp, _provider) = provider();
        let zip = node_zip();
        let provider = RuntimeProvider {
            cache_root: temp.path().join("runtimes"),
            system_node: Arc::new(|| None),
            downloader: Arc::new(MemoryDownloader {
                bytes: BTreeMap::from([("https://example.test/node.zip".to_owned(), zip)]),
            }),
            max_versions: 4,
        };
        let package = RuntimePackage {
            version: "22.14.0".to_owned(),
            url: "https://example.test/node.zip".to_owned(),
            sha256: "0".repeat(64),
        };
        let error = provider
            .install(ServiceRuntime::Node, &TargetTriple::host(), &package)
            .unwrap_err();
        assert!(matches!(error, RuntimeProviderError::Verify(_)), "{error}");
    }

    #[test]
    fn eviction_drops_oldest_versions_beyond_the_cap() {
        let (temp, provider) = provider();
        let root = temp.path().join("runtimes");
        for version in ["21.0.0", "22.0.0", "23.0.0", "24.0.0", "25.0.0"] {
            let dir = root
                .join("node")
                .join(version)
                .join(TargetTriple::host().dir_name());
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join(MANIFEST_FILE), "{}").unwrap();
        }
        provider.evict(ServiceRuntime::Node);
        let remaining: Vec<String> = fs::read_dir(root.join("node"))
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(remaining.len(), 4, "eviction should cap at 4 versions");
    }

    #[test]
    fn installed_lists_only_manifested_entries() {
        let (temp, provider) = provider();
        let root = temp.path().join("runtimes");
        let dir = root.join("node").join("22.14.0").join(TargetTriple::host().dir_name());
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(MANIFEST_FILE), "{}").unwrap();
        // A sibling without a manifest must be ignored.
        let stray = root.join("node").join("9.9.9").join(TargetTriple::host().dir_name());
        fs::create_dir_all(&stray).unwrap();

        let installed = provider.installed(ServiceRuntime::Node);
        assert_eq!(installed.len(), 1);
        assert_eq!(installed[0].version, "22.14.0");
    }
}
