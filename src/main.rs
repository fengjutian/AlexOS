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
    runtime::{RuntimeHandle, RuntimeProcess},
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
    },
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
    },
    /// Run an installed plugin's backend (no webview).
    Plugin {
        id: String,
        #[arg(long, default_value = "./target/apps")]
        install_root: PathBuf,
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
    },
    Revoke {
        id: String,
        permission: String,
        #[arg(long)]
        root: PathBuf,
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

fn execute() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Create { path, id } => {
            package::create_project(&path, &id)?;
            println!("created {}", path.display());
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
            let mut runtime = RuntimeProcess::start(&path, backend)?;
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
            let app = load_app(&path)?;
            shell::run(&path, app)?;
        }
        Commands::Dev { path } => {
            let app = load_app(&path)?;
            dev::run(&path, app)?;
        }
        Commands::Manager { install_root } => {
            // Self-hosting path: if `com.alex.manager` is installed as a
            // plugin, prefer it. The plugin's backend then drives
            // `system.*` calls through the host's permission pipeline.
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
                plugin::run(&plugin_path, &manifest, &install_root)?;
            } else {
                let manager = LocalAppManager::open(&install_root)?;
                let router = Arc::new(ManagerRouter::new(Arc::new(manager)));
                manager_webview::run(router)?;
            }
        }
        Commands::Plugin { id, install_root } => {
            let install_path = plugin::find_in_install(&install_root, &id)?.ok_or_else(|| {
                package::PackageError::NotInstalled(format!(
                    "plugin {id} not found or not a plugin"
                ))
            })?;
            let manifest = load_app(&install_path)?;
            plugin::run(&install_path, &manifest, &install_root)?;
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
            } => {
                PermissionStore::open_at(&root, &id)?
                    .set(&permission, PermissionDecision::Granted)?;
                println!("granted {} to {}", permission, id);
            }
            PermissionCommands::Revoke {
                id,
                permission,
                root,
            } => {
                PermissionStore::open_at(&root, &id)?
                    .set(&permission, PermissionDecision::Denied)?;
                println!("revoked {} from {}", permission, id);
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
