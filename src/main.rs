use std::{path::PathBuf, thread, time::Duration};

use std::sync::Arc;

use alex::{
    api::ApiRouter,
    authorization::{PermissionDecision, PermissionStore},
    dev,
    ipc::Request,
    load_app,
    manager::{LocalAppManager, ManagerRouter},
    manager_webview, package, plugin,
    runtime::{RuntimeHandle, RuntimeProcess, RuntimeSpec, compute_app_dirs},
    shell,
    trust::TrustStore,
    update::{self, UpdateChannel},
};
use clap::{Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(name = "alex", version, about = "Alex OS developer CLI")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Create a new Alex application project.
    Create {
        path: PathBuf,
        #[arg(long)]
        id: String,
        /// Scaffold template. `vanilla` is the plain HTML +
        /// Node.js layout (default). `react-ts` generates a
        /// Vite + React + TypeScript frontend that `alex
        /// build` will bundle into the package root.
        #[arg(long, default_value = "vanilla")]
        template: String,
    },
    /// Build the frontend declared in the manifest. The
    /// build command and arguments come from
    /// `frontend.build` in `manifest.json`; this is a thin
    /// wrapper that runs them from the `frontend/` directory
    /// so the framework's toolchain (Vite, webpack, etc.)
    /// resolves `package.json` correctly.
    Build { path: PathBuf },
    /// Validate an Alex application package.
    Validate { path: PathBuf },
    /// Show the normalized application manifest.
    Inspect { path: PathBuf },
    /// Start the application's managed backend runtime.
    Run { path: PathBuf },
    /// Open the application frontend in the native WebView shell.
    Shell { path: PathBuf },
    /// Run the application in development mode with file watching and hot reload.
    Dev { path: PathBuf },
    /// Open the system App Manager (install, list, uninstall, permissions).
    Manager {
        #[arg(long, default_value = "./target/apps")]
        install_root: PathBuf,
        /// Trust store root. When supplied, the manager upgrades a
        /// `signed-untrusted` package to `signed-trusted` if the
        /// publisher fingerprint is in this store.
        #[arg(long)]
        trust_root: Option<PathBuf>,
    },
    /// Run an installed plugin. Default opens a WebView so the
    /// plugin behaves like an app (frontend + backend + system
    /// permissions) — this is the path that lets a plugin replace
    /// the built-in App Manager. Pass `--headless` to run backend
    /// only with output forwarded to host stdout.
    Plugin {
        id: String,
        #[arg(long, default_value = "./target/apps")]
        install_root: PathBuf,
        /// Trust store root (see `alex manager --help`).
        #[arg(long)]
        trust_root: Option<PathBuf>,
        /// Run the plugin without a WebView (backend only).
        #[arg(long)]
        headless: bool,
    },
    /// Invoke an Alex API request from a JSON file (diagnostic command).
    Invoke {
        path: PathBuf,
        request: PathBuf,
        #[arg(long)]
        timeout_ms: Option<u64>,
    },
    /// Build a validated .alex application archive.
    Pack {
        path: PathBuf,
        output: PathBuf,
        #[arg(long)]
        sign: Option<PathBuf>,
    },
    /// Generate an Ed25519 publisher key file.
    Keygen { output: PathBuf },
    /// Install a .alex archive into an application directory.
    Install {
        archive: PathBuf,
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        require_signature: bool,
        #[arg(long)]
        trusted_key: Option<String>,
        #[arg(long)]
        trust_root: Option<PathBuf>,
    },
    /// Atomically replace an installed app with a verified .alex package.
    Update {
        archive: PathBuf,
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        require_signature: bool,
        #[arg(long)]
        trusted_key: Option<String>,
        #[arg(long)]
        trust_root: Option<PathBuf>,
        #[arg(long)]
        allow_downgrade: bool,
    },
    /// Create a signed channel update manifest for an existing .alex package.
    PublishUpdate {
        package: PathBuf,
        output: PathBuf,
        #[arg(long)]
        key: PathBuf,
        #[arg(long)]
        id: String,
        #[arg(long)]
        version: String,
        #[arg(long)]
        url: String,
        #[arg(long, value_enum, default_value = "stable")]
        channel: CliChannel,
    },
    /// Download a signed update manifest and atomically update an installed app.
    UpdateRemote {
        manifest_url: String,
        #[arg(long)]
        id: String,
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        trust_root: PathBuf,
        #[arg(long, value_enum, default_value = "stable")]
        channel: CliChannel,
    },
    /// List valid applications in an installation directory.
    List {
        #[arg(long)]
        root: PathBuf,
    },
    /// Uninstall an application after validating its identity and path.
    Uninstall {
        id: String,
        #[arg(long)]
        root: PathBuf,
    },
    /// Inspect or change persisted application permission decisions.
    Permissions {
        #[command(subcommand)]
        action: PermissionCommands,
    },
    /// Manage trusted package publisher keys.
    Trust {
        #[command(subcommand)]
        action: TrustCommands,
    },
    /// Diagnose host prerequisites (WebView2 runtime, Node, etc.).
    Doctor,
    /// Run the long-lived Alex Runtime control daemon.
    Daemon {
        /// Durable desired-state file.
        #[arg(long, default_value = "./target/alexd/state.json")]
        state: PathBuf,
        /// Windows named-pipe endpoint.
        #[arg(long, default_value = alex::daemon::DEFAULT_PIPE_NAME)]
        pipe: String,
        /// Installed application root controlled by this daemon.
        #[arg(long, default_value = "./target/apps")]
        install_root: PathBuf,
        /// Permission and audit state root.
        #[arg(long, default_value = "./target/alexd/permissions")]
        permissions_root: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum PermissionCommands {
    List {
        id: String,
        #[arg(long)]
        root: PathBuf,
    },
    Grant {
        id: String,
        permission: String,
        #[arg(long)]
        root: PathBuf,
        /// Install a session-scoped "Allow Once" grant. The
        /// decision is held in memory only and is dropped when
        /// the host's `PermissionStore` is dropped. This is
        /// what the first-use prompt dialog writes when the
        /// user picks "Allow Once".
        #[arg(long)]
        transient: bool,
    },
    Revoke {
        id: String,
        permission: String,
        #[arg(long)]
        root: PathBuf,
        /// Wipe every persisted decision for this app. The
        /// `permission` argument is ignored when this is set.
        /// Use to fully reset an app's permission state
        /// without uninstalling it.
        #[arg(long)]
        all: bool,
    },
    /// Show the most recent permission decisions for an app from
    /// the audit log (JSONL). `transient` ("Allow Once") grants
    /// are not audited by design, so this only reflects the
    /// persisted history.
    Audit {
        id: String,
        #[arg(long)]
        root: PathBuf,
        /// Cap the number of records displayed. Defaults to 50;
        /// pass 0 to read the whole log.
        #[arg(long, default_value = "50")]
        limit: usize,
    },
}

#[derive(Debug, Subcommand)]
enum TrustCommands {
    Add {
        label: String,
        public_key: String,
        #[arg(long)]
        root: PathBuf,
    },
    List {
        #[arg(long)]
        root: PathBuf,
    },
    Remove {
        fingerprint: String,
        #[arg(long)]
        root: PathBuf,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliChannel {
    Stable,
    Beta,
    Dev,
}

impl From<CliChannel> for UpdateChannel {
    fn from(value: CliChannel) -> Self {
        match value {
            CliChannel::Stable => Self::Stable,
            CliChannel::Beta => Self::Beta,
            CliChannel::Dev => Self::Dev,
        }
    }
}

fn main() {
    if let Err(error) = execute() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

/// Format a Unix epoch in milliseconds as an ISO-8601 / UTC
/// string. Used by `alex permissions audit` so each line is
/// sortable with `sort -k1` and unambiguous across time zones.
///
/// Returned in the shape `YYYY-MM-DDTHH:MM:SSZ` so it stays a
/// single field per line in tab-separated output.
fn format_unix_millis_iso8601(millis: u64) -> String {
    use std::time::{Duration, UNIX_EPOCH};
    let Some(dt) = UNIX_EPOCH.checked_add(Duration::from_millis(millis)) else {
        return "0000-00-00T00:00:00Z".into();
    };
    let secs = dt
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Manual breakdown so we don't need a date crate. Good
    // enough for the audit log; sub-second precision is dropped.
    let (year, month, day, hour, minute, second) = epoch_seconds_to_calendar(secs);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hour, minute, second
    )
}

/// Civil-from-days / days-from-civil (Howard Hinnant's
/// `days_from_civil`, public domain). Avoids pulling in
/// `chrono` for what is a one-off formatting helper.
fn epoch_seconds_to_calendar(secs: u64) -> (i32, u32, u32, u32, u32, u32) {
    let days = (secs / 86_400) as i64;
    let secs_of_day = (secs % 86_400) as u32;
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;
    let (y, m, d) = days_from_civil(days);
    (y, m as u32, d as u32, hour, minute, second)
}

fn days_from_civil(z: i64) -> (i32, i32, i32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as i32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as i32;
    let y = if m <= 2 { y + 1 } else { y } as i32;
    (y, m, d)
}

/// Pre-flight check used by every command that opens a WebView.
/// `wry` / WebView2 itself will fail with a cryptic COM error when
/// the runtime is missing; this gives the user a single, actionable
/// message instead. Detection is the same code path as
/// `alex doctor`, so the surfaced message is identical.
fn require_webview2() -> Result<(), Box<dyn std::error::Error>> {
    if let Err(error) = alex::webview2::detect() {
        eprintln!("error: {error}");
        return Err(error.into());
    }
    Ok(())
}

fn execute() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Daemon {
            state,
            pipe,
            install_root,
            permissions_root,
        } => {
            eprintln!("alexd: listening on {pipe}");
            let manager = Arc::new(LocalAppManager::open_with(&install_root, permissions_root)?);
            alex::daemon::run_server(&state, &pipe, manager)?;
        }
        Commands::Create { path, id, template } => {
            let parsed = package::Template::parse(&template);
            package::create_project_with_template(&path, &id, parsed)?;
            println!(
                "created {} (template: {:?})\n  next: cd {} && alex dev .",
                path.display(),
                parsed,
                path.display()
            );
        }
        Commands::Build { path } => {
            package::build_frontend(&path)?;
            println!("built frontend at {}", path.display());
        }
        Commands::Validate { path } => {
            let app = load_app(&path)?;
            println!("valid: {} {} ({})", app.name, app.version, app.id);
        }
        Commands::Inspect { path } => {
            let app = load_app(&path)?;
            println!("{}", serde_json::to_string_pretty(&app)?);
        }
        Commands::Run { path } => {
            let app = load_app(&path)?;
            println!("starting {} {}", app.name, app.version);
            let Some(backend) = &app.backend else {
                println!("application has no backend runtime");
                return Ok(());
            };
            // Build the same auto-managed data / cache / log dir tree
            // that `RuntimeHandle::start_with_spec` would, so the
            // backend sees `ALEX_APP_DATA_DIR` / `ALEX_APP_CACHE_DIR`
            // / `ALEX_APP_LOG_DIR` injected into its env. Without
            // this, `node:sqlite` backends would silently fall back
            // to `:memory:` because `dbPath` would be null.
            let spec = RuntimeSpec {
                app_id: app.id.clone(),
                package_root: path.clone(),
                backend: backend.clone(),
                data_dir: None,
                cache_dir: None,
            };
            let auto_dirs = compute_app_dirs(&app.id).ok();
            let (data_dir, cache_dir, log_dir) = match &auto_dirs {
                Some(dirs) => {
                    dirs.ensure().map_err(|e| format!("ensure app dirs: {e}"))?;
                    (
                        Some(dirs.data.as_path()),
                        Some(dirs.cache.as_path()),
                        Some(dirs.logs.as_path()),
                    )
                }
                None => (None, None, None),
            };
            let logs =
                std::sync::Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()));
            let (mut runtime, endpoint) = RuntimeProcess::start_with_spec(
                &spec,
                data_dir,
                cache_dir,
                log_dir,
                std::sync::Arc::clone(&logs),
            )?;
            if let Some(ep) = &endpoint {
                println!("service endpoint: 127.0.0.1:{}", ep.port);
            }
            println!("runtime started (pid {})", runtime.id());
            loop {
                if let Some(status) = runtime.try_wait()? {
                    if !status.success() {
                        return Err(format!("runtime crashed with {status}").into());
                    }
                    println!("runtime exited successfully");
                    break;
                }
                thread::sleep(Duration::from_millis(100));
            }
        }
        Commands::Shell { path } => {
            require_webview2()?;
            let app = load_app(&path)?;
            shell::run(&path, app, None, None)?;
        }
        Commands::Dev { path } => {
            require_webview2()?;
            let app = load_app(&path)?;
            dev::run(&path, app)?;
        }
        Commands::Manager {
            install_root,
            trust_root,
        } => {
            require_webview2()?;
            // Self-hosting path: if `com.alex.manager` is installed as a
            // plugin, prefer it. We delegate to `shell::run` (same path
            // as `alex plugin <id>` without `--headless`) so the plugin
            // gets a real WebView to render its frontend; the WebView
            // talks to the host through the regular `window.alex`
            // transport, and the host-side `ApiRouter` enforces the
            // plugin's `system.*` permissions. `plugin::run` is the
            // reverse-IPC path (backend asks host a question) and
            // does NOT open a WebView, so it is not appropriate for a
            // user-facing manager UI.
            //
            // Fallback: built-in `ManagerRouter` keeps 0.1 working
            // before users have installed the manager plugin.
            if let Ok(Some(plugin_path)) =
                plugin::find_in_install(&install_root, "com.alex.manager")
            {
                let manifest = load_app(&plugin_path)?;
                eprintln!(
                    "alex manager: launching self-hosted plugin {} {}",
                    manifest.id, manifest.version
                );
                shell::run(
                    &plugin_path,
                    manifest,
                    Some(&install_root),
                    trust_root.as_deref(),
                )?;
            } else {
                // Built-in manager fallback: pass the trust root
                // through so the UI's signature badges can show
                // "signed-trusted" when the fingerprint is in the
                // store.
                let permissions_root = std::env::var_os("ALEX_DATA_DIR")
                    .map(PathBuf::from)
                    .or_else(|| {
                        std::env::var_os("LOCALAPPDATA").map(|p| PathBuf::from(p).join("AlexOS"))
                    })
                    .unwrap_or_else(|| install_root.clone());
                let manager =
                    LocalAppManager::open_with_trust(&install_root, permissions_root, trust_root)?;
                let router = Arc::new(ManagerRouter::new(Arc::new(manager)));
                manager_webview::run(router)?;
            }
        }
        Commands::Plugin {
            id,
            install_root,
            trust_root: _,
            headless,
        } => {
            let install_path = plugin::find_in_install(&install_root, &id)?.ok_or_else(|| {
                package::PackageError::NotInstalled(format!(
                    "plugin {id} not found or not a plugin"
                ))
            })?;
            let manifest = load_app(&install_path)?;
            if headless {
                plugin::run(&install_path, &manifest, &install_root, true)?;
            } else {
                eprintln!(
                    "alex plugin: launching webview for {} {}",
                    manifest.id, manifest.version
                );
                // `alex plugin` has no `--trust-root` flag; the trust
                // store is conventionally co-located with the install
                // root (`<install_root>/publishers.json`).
                shell::run(
                    &install_path,
                    manifest,
                    Some(&install_root),
                    Some(&install_root),
                )?;
            }
        }
        Commands::Invoke {
            path,
            request,
            timeout_ms,
        } => {
            let app = load_app(&path)?;
            let request = std::fs::read_to_string(request)?;
            let mut request: Request = serde_json::from_str(&request)?;
            if let Some(timeout) = timeout_ms {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)?
                    .as_millis() as u64;
                request.deadline_ms = Some(now.saturating_add(timeout));
            }
            let mut router = ApiRouter::new(path.clone(), app.clone());
            if request.method.starts_with("runtime.")
                && let Some(backend) = &app.backend
            {
                router = router.with_runtime(RuntimeHandle::start(&path, backend)?);
            }
            let response = router.dispatch(request);
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Commands::Pack { path, output, sign } => {
            if let Some(key) = sign {
                package::pack_signed(&path, &output, &key)?;
            } else {
                package::pack(&path, &output)?;
            }
            println!("packed {}", output.display());
        }
        Commands::Keygen { output } => {
            let public_key = package::generate_signing_key(&output)?;
            println!("generated {}\npublic key: {}", output.display(), public_key);
        }
        Commands::Install {
            archive,
            root,
            require_signature,
            trusted_key,
            trust_root,
        } => {
            let trusted_key = if let Some(trust_root) = trust_root {
                let signer = package::signer_public_key(&archive)?.ok_or_else(|| {
                    package::PackageError::Signature("package is unsigned".into())
                })?;
                TrustStore::open(&trust_root)?.require(&signer)?;
                Some(signer)
            } else {
                trusted_key
            };
            let installed = package::install_verified(
                &archive,
                &root,
                require_signature,
                trusted_key.as_deref(),
            )?;
            println!("installed {}", installed.display());
        }
        Commands::Update {
            archive,
            root,
            require_signature,
            trusted_key,
            trust_root,
            allow_downgrade,
        } => {
            let trusted_key = resolve_trusted_key(&archive, trusted_key, trust_root)?;
            let updated = package::update_verified(
                &archive,
                &root,
                require_signature || trusted_key.is_some(),
                trusted_key.as_deref(),
                allow_downgrade,
            )?;
            println!(
                "updated {} {} -> {} ({})",
                updated.id,
                updated.previous_version,
                updated.version,
                updated.path.display()
            );
            if updated.backup_retained {
                eprintln!("warning: the old-version backup could not be removed");
            }
        }
        Commands::PublishUpdate {
            package,
            output,
            key,
            id,
            version,
            url,
            channel,
        } => {
            let manifest =
                update::manifest_for_package(id, channel.into(), version, url, &package)?;
            let signed = update::create_signed_manifest(manifest, &key)?;
            let package_signer = package::signer_public_key(&package)?.ok_or_else(|| {
                package::PackageError::Signature("update package is unsigned".into())
            })?;
            if package_signer != signed.public_key {
                return Err(package::PackageError::Signature(
                    "update manifest and package use different publisher keys".into(),
                )
                .into());
            }
            std::fs::write(&output, serde_json::to_vec_pretty(&signed)?)?;
            println!("published update manifest {}", output.display());
        }
        Commands::UpdateRemote {
            manifest_url,
            id,
            root,
            trust_root,
            channel,
        } => {
            let result =
                update::update_from_url(&manifest_url, &root, &id, channel.into(), &trust_root)?;
            println!(
                "updated {} {} -> {} ({})",
                result.id,
                result.previous_version,
                result.version,
                result.path.display()
            );
        }
        Commands::List { root } => {
            let applications = package::list_installed(&root)?;
            if applications.is_empty() {
                println!("no applications installed");
            } else {
                for app in applications {
                    println!(
                        "{}\t{}\t{}\t{}",
                        app.id,
                        app.version,
                        app.name,
                        app.path.display()
                    );
                }
            }
        }
        Commands::Uninstall { id, root } => {
            let removed = package::uninstall(&id, &root)?;
            println!("uninstalled {} ({})", id, removed.display());
        }
        Commands::Permissions { action } => match action {
            PermissionCommands::List { id, root } => {
                let store = PermissionStore::open_at(&root, &id)?;
                for (permission, decision) in store.list() {
                    println!("{}\t{:?}", permission, decision);
                }
            }
            PermissionCommands::Grant {
                id,
                permission,
                root,
                transient,
            } => {
                let store = PermissionStore::open_at(&root, &id)?;
                if transient {
                    // The transient grant lives until this
                    // process exits. CLI users typically run a
                    // single alex invocation, so the grant
                    // effectively scopes to that invocation.
                    store.set_transient(&permission, PermissionDecision::Granted);
                    println!(
                        "granted (transient) {} to {} for this session",
                        permission, id
                    );
                } else {
                    store.set(&permission, PermissionDecision::Granted)?;
                    println!("granted {} to {}", permission, id);
                }
            }
            PermissionCommands::Revoke {
                id,
                permission,
                root,
                all,
            } => {
                let store = PermissionStore::open_at(&root, &id)?;
                if all {
                    let cleared = store.revoke_all()?;
                    println!("revoked all ({cleared}) permissions from {id}");
                } else {
                    store.set(&permission, PermissionDecision::Denied)?;
                    println!("revoked {} from {}", permission, id);
                }
            }
            PermissionCommands::Audit { id, root, limit } => {
                let store = PermissionStore::open_at(&root, &id)?;
                let report = store.recent_audit(limit)?;
                if report.entries.is_empty() && report.skipped == 0 {
                    println!("(no audit entries for {id})");
                } else {
                    // Tab-separated; pipes cleanly into cut/awk.
                    println!("timestamp\tpermission\tdecision");
                    for entry in &report.entries {
                        // ISO-8601 / UTC: 2026-08-22T12:30:45Z.
                        // Kept on one line so the file stays
                        // sortable with `sort -k1`.
                        let iso = format_unix_millis_iso8601(entry.timestamp_ms);
                        println!("{}\t{}\t{:?}", iso, entry.permission, entry.decision);
                    }
                    if report.skipped > 0 {
                        eprintln!(
                            "warning: {} audit line(s) skipped (malformed JSON)",
                            report.skipped
                        );
                    }
                }
            }
        },
        Commands::Trust { action } => match action {
            TrustCommands::Add {
                label,
                public_key,
                root,
            } => {
                let fingerprint = TrustStore::open(&root)?.add(label, public_key)?;
                println!("trusted publisher {}", fingerprint);
            }
            TrustCommands::List { root } => {
                for (fingerprint, publisher) in TrustStore::open(&root)?.list() {
                    println!(
                        "{}\t{}\t{}",
                        fingerprint, publisher.label, publisher.public_key
                    );
                }
            }
            TrustCommands::Remove { fingerprint, root } => {
                if TrustStore::open(&root)?.remove(&fingerprint)? {
                    println!("removed publisher {}", fingerprint);
                } else {
                    println!("publisher not found: {}", fingerprint);
                }
            }
        },
        Commands::Doctor => run_doctor()?,
    }
    Ok(())
}

fn run_doctor() -> Result<(), Box<dyn std::error::Error>> {
    println!("Alex OS host diagnostics\n");

    // WebView2 — required for every page render.
    print!("  WebView2 Runtime ... ");
    match alex::webview2::detect() {
        Ok(status) => {
            println!("ok");
            println!("    name        : {}", status.name);
            println!("    version     : {}", status.version);
            println!("    install path: {}", status.install_path.display());
            println!("    registry    : {}", status.source.as_reg_path());
        }
        Err(alex::webview2::WebView2Error::NotInstalled) => {
            println!("MISSING");
            println!();
            println!("    Alex OS renders every page through WebView2 and");
            println!("    cannot start without it. Install the Evergreen");
            println!("    Bootstrapper from:");
            println!("      {}", alex::webview2::WEBVIEW2_BOOTSTRAP_URL);
            println!("    then re-run `alex doctor`.");
            return Err(alex::webview2::WebView2Error::NotInstalled.into());
        }
        Err(other) => {
            println!("ERROR");
            println!("    {other}");
            return Err(other.into());
        }
    }

    // Node — required for app backends.
    print!("  Node.js .......... ");
    let node_result = std::process::Command::new("node").arg("--version").output();
    match node_result {
        Ok(out) if out.status.success() => {
            let v = String::from_utf8_lossy(&out.stdout);
            println!("ok ({})", v.trim());
        }
        _ => {
            println!("NOT FOUND");
            println!("    App backends expect `node` on PATH (or set");
            println!("    `ALEX_NODE`). Install Node.js or set the env var.");
        }
    }

    Ok(())
}

fn resolve_trusted_key(
    archive: &std::path::Path,
    explicit: Option<String>,
    trust_root: Option<PathBuf>,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    if let Some(trust_root) = trust_root {
        let signer = package::signer_public_key(archive)?
            .ok_or_else(|| package::PackageError::Signature("package is unsigned".into()))?;
        TrustStore::open(&trust_root)?.require(&signer)?;
        Ok(Some(signer))
    } else {
        Ok(explicit)
    }
}
