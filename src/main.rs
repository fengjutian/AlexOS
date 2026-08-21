use std::{path::PathBuf, thread, time::Duration};

use alex::{
    api::ApiRouter,
    ipc::Request,
    load_app, package,
    runtime::{RuntimeHandle, RuntimeProcess},
    shell,
};
use clap::{Parser, Subcommand};

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
    /// Invoke an Alex API request from a JSON file (diagnostic command).
    Invoke { path: PathBuf, request: PathBuf },
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
        Commands::Invoke { path, request } => {
            let app = load_app(&path)?;
            let request = std::fs::read_to_string(request)?;
            let request: Request = serde_json::from_str(&request)?;
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
        } => {
            let installed = package::install_verified(
                &archive,
                &root,
                require_signature,
                trusted_key.as_deref(),
            )?;
            println!("installed {}", installed.display());
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
    }
    Ok(())
}
