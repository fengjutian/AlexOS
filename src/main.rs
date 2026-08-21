use std::{path::PathBuf, thread, time::Duration};

use alex::{load_app, runtime::RuntimeProcess};
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "alex", version, about = "Alex OS developer CLI")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Validate an Alex application package.
    Validate { path: PathBuf },
    /// Show the normalized application manifest.
    Inspect { path: PathBuf },
    /// Start the application's managed backend runtime.
    Run { path: PathBuf },
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
    }
    Ok(())
}
