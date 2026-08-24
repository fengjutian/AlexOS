use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand_core::OsRng;
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::Builder as TempDirBuilder;
use thiserror::Error;
use zip::{ZipArchive, ZipWriter, write::SimpleFileOptions};

use crate::{
    AlexError, load_app,
    manifest::{AppManifest, UpdateSource},
    manifest_v2::{self, ManifestV2Error},
};

const INTEGRITY_PATH: &str = ".alex/integrity.json";
const SIGNATURE_PATH: &str = ".alex/signature.json";
const MAX_PACKAGE_FILES: usize = 10_000;
const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum PackageError {
    #[error(transparent)]
    Alex(#[from] AlexError),
    #[error(transparent)]
    ManifestV2(#[from] ManifestV2Error),
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
    #[error("package signature check failed: {0}")]
    Signature(String),
    #[error("package version check failed: {0}")]
    Version(String),
    #[error("update package id {actual} does not match installed app {expected}")]
    IdentityMismatch { expected: String, actual: String },
    #[error("frontend build failed: {0}")]
    Build(String),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct IntegrityManifest {
    algorithm: String,
    files: BTreeMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SignatureManifest {
    algorithm: String,
    public_key: String,
    signature: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct KeyFile {
    algorithm: String,
    public_key: String,
    secret_key: String,
}

#[derive(Debug)]
pub struct InstalledApp {
    pub id: String,
    pub name: String,
    pub version: String,
    pub path: PathBuf,
    pub update: Option<crate::manifest::UpdateSource>,
}

#[derive(Debug)]
struct PackageMetadata {
    id: String,
    name: String,
    version: String,
    update: Option<UpdateSource>,
}

fn package_metadata(root: &Path) -> Result<PackageMetadata, PackageError> {
    let has_v1 = root.join("manifest.json").is_file();
    let has_v2 = root.join("app.yaml").is_file();
    match (has_v1, has_v2) {
        (true, true) => Err(PackageError::Integrity(
            "package contains both manifest.json and app.yaml".into(),
        )),
        (true, false) => {
            let manifest = load_app(root)?;
            Ok(PackageMetadata {
                id: manifest.id,
                name: manifest.name,
                version: manifest.version,
                update: manifest.update,
            })
        }
        (false, true) => {
            let manifest = manifest_v2::load(root)?;
            Ok(PackageMetadata {
                id: manifest.id,
                name: manifest.name,
                version: manifest.version,
                update: None,
            })
        }
        (false, false) => Err(PackageError::Integrity(
            "package has neither manifest.json nor app.yaml".into(),
        )),
    }
}

#[derive(Debug)]
pub struct UpdateResult {
    pub id: String,
    pub previous_version: String,
    pub version: String,
    pub path: PathBuf,
    pub backup_retained: bool,
}

/// Template the scaffolder should generate. `Vanilla` is the
/// existing plain-HTML + Node.js backend shape. `ReactTs` adds
/// a Vite + React + TypeScript frontend whose build output is
/// loaded by the host at runtime; see [`create_react_ts`] for
/// the file layout and [`build_frontend`] for the build step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Template {
    Vanilla,
    ReactTs,
}

impl Template {
    /// Parse the CLI string. Defaults to `Vanilla` for the empty
    /// string so existing `--template ""` (and "default") usage
    /// keeps working.
    pub fn parse(value: &str) -> Self {
        match value {
            "react-ts" | "react" => Self::ReactTs,
            _ => Self::Vanilla,
        }
    }
}

pub fn create_project(destination: &Path, package_id: &str) -> Result<(), PackageError> {
    create_project_with_template(destination, package_id, Template::Vanilla)
}

pub fn create_project_with_template(
    destination: &Path,
    package_id: &str,
    template: Template,
) -> Result<(), PackageError> {
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
    match template {
        Template::Vanilla => create_vanilla(destination, name, package_id)?,
        Template::ReactTs => create_react_ts(destination, name, package_id)?,
    }
    load_app(destination)?;
    Ok(())
}

fn create_vanilla(destination: &Path, name: &str, package_id: &str) -> Result<(), PackageError> {
    let manifest = serde_json::json!({
        "schemaVersion": 1,
        "id": package_id,
        "name": name,
        "version": "0.1.0",
        "frontend": { "entry": "frontend/index.html" },
        "backend": { "runtime": "node", "entry": "backend/index.js" },
        "permissions": [{ "name": "runtime.invoke" }, { "name": "runtime.manage" }]
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
        "const readline=require('node:readline');\nconst input=readline.createInterface({input:process.stdin});\ninput.on('line',line=>{const r=JSON.parse(line);if(r.type==='shutdown'){input.close();return;}process.stdout.write(JSON.stringify({protocol:1,id:r.id,result:{ok:true}})+'\\n')});\n",
    )?;
    Ok(())
}

fn create_react_ts(destination: &Path, name: &str, package_id: &str) -> Result<(), PackageError> {
    // The manifest's frontend.entry points at the *built* output
    // because the host serves static files. The dev template
    // source lives in frontend/src/ but is loaded by `npm run
    // build` from frontend/. The `frontend.build` block tells
    // `alex build` how to invoke it without assuming npm vs pnpm
    // vs yarn.
    let manifest = serde_json::json!({
        "schemaVersion": 1,
        "id": package_id,
        "name": name,
        "version": "0.1.0",
        "frontend": {
            "entry": "frontend/dist/index.html",
            "build": {
                "command": "npm",
                "args": ["run", "build"]
            }
        },
        "backend": { "runtime": "node", "entry": "backend/index.js" },
        "permissions": [{ "name": "runtime.invoke" }, { "name": "runtime.manage" }]
    });
    fs::write(
        destination.join("manifest.json"),
        format!(
            "{}\n",
            serde_json::to_string_pretty(&manifest).expect("JSON value is valid")
        ),
    )?;
    write_file(destination.join("frontend/index.html"), REACT_TS_INDEX_HTML)?;
    write_file(destination.join("frontend/src/main.tsx"), REACT_TS_MAIN_TSX)?;
    write_file(destination.join("frontend/src/App.tsx"), REACT_TS_APP_TSX)?;
    write_file(
        destination.join("frontend/package.json"),
        REACT_TS_PACKAGE_JSON,
    )?;
    write_file(
        destination.join("frontend/tsconfig.json"),
        REACT_TS_TSCONFIG,
    )?;
    write_file(
        destination.join("frontend/vite.config.ts"),
        REACT_TS_VITE_CONFIG,
    )?;
    write_file(
        destination.join("frontend/.alexignore"),
        REACT_TS_ALEXIGNORE,
    )?;
    write_file(destination.join("frontend/README.md"), REACT_TS_README)?;
    // Keep a freshly scaffolded project structurally valid before the first
    // frontend build. Vite replaces this placeholder on `alex build`.
    write_file(
        destination.join("frontend/dist/index.html"),
        "<!doctype html><meta charset=\"utf-8\"><title>Build required</title><p>Run alex build to compile this React application.</p>\n",
    )?;
    // Backend is the same JSON-RPC echo stub as the vanilla
    // template so the dev host can spawn it without Node-tooling
    // config on top of the Vite tooling.
    fs::write(
        destination.join("backend/index.js"),
        "const readline=require('node:readline');\nconst input=readline.createInterface({input:process.stdin});\ninput.on('line',line=>{const r=JSON.parse(line);if(r.type==='shutdown'){input.close();return;}process.stdout.write(JSON.stringify({protocol:1,id:r.id,result:{ok:true}})+'\\n')});\n",
    )?;
    let _ = name; // currently unused; reserved for the README heading
    Ok(())
}

fn write_file(path: PathBuf, body: &str) -> Result<(), PackageError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, body)?;
    Ok(())
}

pub fn pack(source: &Path, output: &Path) -> Result<(), PackageError> {
    pack_internal(source, output, None)
}

pub fn generate_signing_key(output: &Path) -> Result<String, PackageError> {
    if output.exists() {
        return Err(PackageError::AlreadyInstalled(output.to_path_buf()));
    }
    let signing = SigningKey::generate(&mut OsRng);
    let public_key = BASE64.encode(signing.verifying_key().to_bytes());
    let key = KeyFile {
        algorithm: "ed25519".into(),
        public_key: public_key.clone(),
        secret_key: BASE64.encode(signing.to_bytes()),
    };
    fs::write(
        output,
        serde_json::to_vec_pretty(&key).expect("key data is valid"),
    )?;
    Ok(public_key)
}

pub fn pack_signed(source: &Path, output: &Path, key_path: &Path) -> Result<(), PackageError> {
    let key: KeyFile = serde_json::from_reader(File::open(key_path)?)
        .map_err(|error| PackageError::Signature(format!("invalid key file: {error}")))?;
    if key.algorithm != "ed25519" {
        return Err(PackageError::Signature("unsupported key algorithm".into()));
    }
    let secret: [u8; 32] = BASE64
        .decode(&key.secret_key)
        .map_err(|error| PackageError::Signature(error.to_string()))?
        .try_into()
        .map_err(|_| PackageError::Signature("invalid secret key length".into()))?;
    let signing = SigningKey::from_bytes(&secret);
    if BASE64.encode(signing.verifying_key().to_bytes()) != key.public_key {
        return Err(PackageError::Signature(
            "public and secret key do not match".into(),
        ));
    }
    pack_internal(source, output, Some(&signing))
}

pub fn sign_payload(key_path: &Path, payload: &[u8]) -> Result<(String, String), PackageError> {
    let key: KeyFile = serde_json::from_reader(File::open(key_path)?)
        .map_err(|error| PackageError::Signature(format!("invalid key file: {error}")))?;
    let secret: [u8; 32] = BASE64
        .decode(&key.secret_key)
        .map_err(|error| PackageError::Signature(error.to_string()))?
        .try_into()
        .map_err(|_| PackageError::Signature("invalid secret key length".into()))?;
    let signing = SigningKey::from_bytes(&secret);
    let public_key = BASE64.encode(signing.verifying_key().to_bytes());
    if public_key != key.public_key {
        return Err(PackageError::Signature(
            "public and secret key do not match".into(),
        ));
    }
    Ok((public_key, BASE64.encode(signing.sign(payload).to_bytes())))
}

// =====================================================================
// React + TypeScript template (Vite + React 18)
//
// Constants live at the bottom so the long function bodies above
// stay readable. All the strings are written verbatim by
// `create_react_ts` — no template substitution, so the file
// content is stable and unit-testable byte-for-byte.
// =====================================================================

const REACT_TS_INDEX_HTML: &str = r#"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <title>Alex OS App</title>
    <link rel="icon" href="data:," />
  </head>
  <body>
    <div id="root"></div>
    <script type="module" src="/src/main.tsx"></script>
  </body>
</html>
"#;

const REACT_TS_MAIN_TSX: &str = r#"import React from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";

const container = document.getElementById("root");
if (!container) throw new Error("missing #root element in index.html");
createRoot(container).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>
);
"#;

const REACT_TS_APP_TSX: &str = r#"import { useEffect, useState } from "react";

declare global {
  interface Window {
    alex: {
      invoke<T = unknown>(method: string, params?: unknown): Promise<T>;
      on(event: string, listener: (data: unknown) => void): () => void;
    };
  }
}

export function App() {
  const [now, setNow] = useState(() => new Date().toISOString());

  useEffect(() => {
    const timer = window.setInterval(() => {
      setNow(new Date().toISOString());
    }, 1000);
    return () => window.clearInterval(timer);
  }, []);

  return (
    <main style={{ fontFamily: "system-ui, sans-serif", padding: 24 }}>
      <h1>Alex OS · React + TypeScript</h1>
      <p>This is a React + TypeScript app running inside the Alex OS WebView.</p>
      <p>
        Use <code>window.alex.invoke("…", {"{…}"})</code> to call host APIs. The
        BRIDGE is injected automatically before this module loads.
      </p>
      <p>Current time: <strong>{now}</strong></p>
    </main>
  );
}
"#;

const REACT_TS_PACKAGE_JSON: &str = r#"{
  "name": "alex-app",
  "private": true,
  "version": "0.1.0",
  "type": "module",
  "scripts": {
    "dev": "vite",
    "build": "tsc --noEmit && vite build",
    "preview": "vite preview"
  },
  "dependencies": {
    "react": "^18.3.1",
    "react-dom": "^18.3.1"
  },
  "devDependencies": {
    "@types/react": "^18.3.12",
    "@types/react-dom": "^18.3.1",
    "@vitejs/plugin-react": "^4.3.3",
    "typescript": "^5.6.3",
    "vite": "^5.4.10"
  }
}
"#;

const REACT_TS_TSCONFIG: &str = r#"{
  "compilerOptions": {
    "target": "ES2022",
    "lib": ["ES2022", "DOM", "DOM.Iterable"],
    "module": "ESNext",
    "moduleResolution": "bundler",
    "jsx": "react-jsx",
    "strict": true,
    "noUnusedLocals": true,
    "noUnusedParameters": true,
    "noFallthroughCasesInSwitch": true,
    "skipLibCheck": true,
    "esModuleInterop": true,
    "allowSyntheticDefaultImports": true,
    "resolveJsonModule": true,
    "isolatedModules": true,
    "moduleDetection": "force",
    "verbatimModuleSyntax": true
  },
  "include": ["src"]
}
"#;

const REACT_TS_VITE_CONFIG: &str = r#"import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Source lives at frontend/src/ with the Vite entry in
// frontend/index.html. `outDir: "dist"` keeps the build output
// in frontend/dist/ so it doesn't clobber the source files
// (Vite warns and `emptyOutDir: true` would wipe them). The
// manifest's `frontend.entry` points at frontend/dist/index.html
// so the Alex OS host serves the built bundle.
export default defineConfig({
  plugins: [react()],
  base: "./",
  build: {
    outDir: "dist",
    emptyOutDir: true,
    sourcemap: true,
  },
});
"#;

const REACT_TS_ALEXIGNORE: &str = r#"# Ignore the build artefacts and Vite cache so `alex dev`
# does not hot-reload when the bundle regenerates.
node_modules
dist
.vite
*.tsbuildinfo
"#;

const REACT_TS_README: &str = r#"# Alex OS · React + TypeScript template

Generated by `alex create --template react-ts`.

## Build

    cd frontend
    npm install
    npm run build

`npm run build` runs `tsc --noEmit` first to type-check, then
`vite build` which emits `frontend/index.html` and
`frontend/assets/*` (the bundled JS/CSS).

## Develop

    alex dev .

`alex dev` watches `frontend/` and reloads the WebView on
change. Run `npm run dev` in a second terminal for the
Vite-side hot reload (HMR), or just edit and let `alex dev`
do a full page reload on save.

## Layout

    manifest.json          Alex OS package manifest
    backend/index.js       Node.js JSON-RPC backend
    frontend/
      index.html           Built entry (served by host)
      src/                 React + TypeScript source
      package.json         Vite + React + TS toolchain
      tsconfig.json
      vite.config.ts
      .alexignore          Files `alex dev` should ignore
"#;

/// Run the manifest-declared frontend build.
///
/// The build shape lives in the manifest under
/// `frontend.build`: `{ command, args }`. We shell out the
/// command from `frontend/` so `package.json` and `node_modules`
/// resolve the way the framework expects.
pub fn build_frontend(destination: &Path) -> Result<(), PackageError> {
    let manifest = load_app(destination)?;
    let build = manifest.frontend.build.as_ref().ok_or_else(|| {
        PackageError::Build("manifest has no frontend.build block; nothing to do".into())
    })?;
    let frontend_dir = destination.join(
        Path::new(&manifest.frontend.entry)
            .parent()
            .unwrap_or(Path::new("frontend")),
    );
    let mut command = std::process::Command::new(&build.command);
    command.args(&build.args).current_dir(&frontend_dir);
    let status = command.status().map_err(|e| {
        PackageError::Build(format!(
            "failed to spawn {} for frontend build: {e}",
            build.command
        ))
    })?;
    if !status.success() {
        return Err(PackageError::Build(format!(
            "frontend build failed with exit code {:?}",
            status.code()
        )));
    }
    Ok(())
}

fn pack_internal(
    source: &Path,
    output: &Path,
    signing: Option<&SigningKey>,
) -> Result<(), PackageError> {
    package_metadata(source)?;
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
    let integrity_bytes = serde_json::to_vec_pretty(&integrity).expect("integrity data is valid");
    writer.start_file(INTEGRITY_PATH, SimpleFileOptions::default())?;
    writer.write_all(&integrity_bytes)?;
    if let Some(signing) = signing {
        let signature = SignatureManifest {
            algorithm: "ed25519".into(),
            public_key: BASE64.encode(signing.verifying_key().to_bytes()),
            signature: BASE64.encode(signing.sign(&integrity_bytes).to_bytes()),
        };
        writer.start_file(SIGNATURE_PATH, SimpleFileOptions::default())?;
        writer
            .write_all(&serde_json::to_vec_pretty(&signature).expect("signature data is valid"))?;
    }
    writer.finish()?;
    Ok(())
}

pub fn install(archive_path: &Path, install_root: &Path) -> Result<PathBuf, PackageError> {
    install_verified(archive_path, install_root, false, None)
}

pub fn install_verified(
    archive_path: &Path,
    install_root: &Path,
    require_signature: bool,
    trusted_key: Option<&str>,
) -> Result<PathBuf, PackageError> {
    fs::create_dir_all(install_root)?;
    let temporary = TempDirBuilder::new()
        .prefix(".alex-install-")
        .tempdir_in(install_root)?;
    let mut archive = ZipArchive::new(File::open(archive_path)?)?;
    if archive.len() > MAX_PACKAGE_FILES + 2 {
        return Err(PackageError::Limit(format!(
            "more than {MAX_PACKAGE_FILES} files"
        )));
    }
    let (integrity, integrity_bytes): (IntegrityManifest, Vec<u8>) = {
        let mut entry = archive
            .by_name(INTEGRITY_PATH)
            .map_err(|_| PackageError::Integrity("missing integrity manifest".into()))?;
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes)?;
        let manifest = serde_json::from_slice(&bytes)
            .map_err(|error| PackageError::Integrity(format!("invalid manifest: {error}")))?;
        (manifest, bytes)
    };
    if integrity.algorithm != "sha256" {
        return Err(PackageError::Integrity(format!(
            "unsupported algorithm {}",
            integrity.algorithm
        )));
    }
    let signature = match archive.by_name(SIGNATURE_PATH) {
        Ok(entry) => Some(
            serde_json::from_reader::<_, SignatureManifest>(entry)
                .map_err(|error| PackageError::Signature(format!("invalid metadata: {error}")))?,
        ),
        Err(zip::result::ZipError::FileNotFound) => None,
        Err(error) => return Err(error.into()),
    };
    verify_signature(
        signature.as_ref(),
        &integrity_bytes,
        require_signature,
        trusted_key,
    )?;
    let mut seen = HashSet::new();
    let mut total_bytes = 0_u64;
    let mut integrity_entries = 0_usize;
    let mut signature_entries = 0_usize;
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
        if relative_name == SIGNATURE_PATH {
            signature_entries += 1;
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
    if signature_entries != usize::from(signature.is_some()) {
        return Err(PackageError::Signature(
            "archive contains duplicate signature metadata".into(),
        ));
    }
    if seen.len() != integrity.files.len() {
        return Err(PackageError::Integrity(
            "integrity manifest references missing files".into(),
        ));
    }
    let manifest = package_metadata(temporary.path())?;
    let destination = install_root.join(&manifest.id);
    if destination.exists() {
        return Err(PackageError::AlreadyInstalled(destination));
    }
    let extracted = temporary.keep();
    fs::rename(extracted, &destination)?;
    Ok(destination)
}

pub fn signer_public_key(archive_path: &Path) -> Result<Option<String>, PackageError> {
    let mut archive = ZipArchive::new(File::open(archive_path)?)?;
    match archive.by_name(SIGNATURE_PATH) {
        Ok(entry) => {
            let signature: SignatureManifest = serde_json::from_reader(entry)
                .map_err(|error| PackageError::Signature(format!("invalid metadata: {error}")))?;
            Ok(Some(signature.public_key))
        }
        Err(zip::result::ZipError::FileNotFound) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub fn archive_identity(archive_path: &Path) -> Result<(String, String), PackageError> {
    let mut archive = ZipArchive::new(File::open(archive_path)?)?;
    let manifest: AppManifest = serde_json::from_reader(
        archive
            .by_name("manifest.json")
            .map_err(|_| PackageError::Integrity("missing manifest.json".into()))?,
    )
    .map_err(|error| PackageError::Integrity(format!("invalid manifest.json: {error}")))?;
    Ok((manifest.id, manifest.version))
}

pub fn update_verified(
    archive_path: &Path,
    install_root: &Path,
    require_signature: bool,
    trusted_key: Option<&str>,
    allow_downgrade: bool,
) -> Result<UpdateResult, PackageError> {
    fs::create_dir_all(install_root)?;
    let staging = TempDirBuilder::new()
        .prefix(".alex-update-")
        .tempdir_in(install_root)?;
    let staged_path =
        install_verified(archive_path, staging.path(), require_signature, trusted_key)?;
    let next = load_app(&staged_path)?;
    let destination = install_root.join(&next.id);
    if !destination.is_dir() {
        return Err(PackageError::NotInstalled(next.id));
    }
    let current = load_app(&destination)?;
    if current.id != next.id {
        return Err(PackageError::IdentityMismatch {
            expected: current.id,
            actual: next.id,
        });
    }
    let current_version = Version::parse(&current.version)
        .map_err(|error| PackageError::Version(format!("installed version: {error}")))?;
    let next_version = Version::parse(&next.version)
        .map_err(|error| PackageError::Version(format!("update version: {error}")))?;
    if !allow_downgrade && next_version <= current_version {
        return Err(PackageError::Version(format!(
            "{} is not newer than {}",
            next.version, current.version
        )));
    }

    let backup = install_root.join(format!(".alex-backup-{}-{}", next.id, std::process::id()));
    if backup.exists() {
        return Err(PackageError::AlreadyInstalled(backup));
    }
    fs::rename(&destination, &backup)?;
    if let Err(error) = fs::rename(&staged_path, &destination) {
        let _ = fs::rename(&backup, &destination);
        return Err(PackageError::Io(error));
    }
    let backup_retained = fs::remove_dir_all(&backup).is_err();
    Ok(UpdateResult {
        id: next.id,
        previous_version: current.version,
        version: next.version,
        path: destination,
        backup_retained,
    })
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
        if let Ok(manifest) = package_metadata(&path) {
            applications.push(InstalledApp {
                id: manifest.id,
                name: manifest.name,
                version: manifest.version,
                update: manifest.update,
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
    let manifest = package_metadata(&destination)?;
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

fn verify_signature(
    metadata: Option<&SignatureManifest>,
    message: &[u8],
    required: bool,
    trusted_key: Option<&str>,
) -> Result<(), PackageError> {
    let Some(metadata) = metadata else {
        if required || trusted_key.is_some() {
            return Err(PackageError::Signature("package is unsigned".into()));
        }
        return Ok(());
    };
    if metadata.algorithm != "ed25519" {
        return Err(PackageError::Signature("unsupported algorithm".into()));
    }
    if trusted_key.is_some_and(|trusted| trusted != metadata.public_key) {
        return Err(PackageError::Signature(
            "publisher key is not trusted".into(),
        ));
    }
    let public_bytes: [u8; 32] = BASE64
        .decode(&metadata.public_key)
        .map_err(|error| PackageError::Signature(error.to_string()))?
        .try_into()
        .map_err(|_| PackageError::Signature("invalid public key length".into()))?;
    let signature_bytes: [u8; 64] = BASE64
        .decode(&metadata.signature)
        .map_err(|error| PackageError::Signature(error.to_string()))?
        .try_into()
        .map_err(|_| PackageError::Signature("invalid signature length".into()))?;
    let verifying = VerifyingKey::from_bytes(&public_bytes)
        .map_err(|error| PackageError::Signature(error.to_string()))?;
    verifying
        .verify(message, &Signature::from_bytes(&signature_bytes))
        .map_err(|error| PackageError::Signature(error.to_string()))
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
