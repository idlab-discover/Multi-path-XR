use std::{path::PathBuf, process::ExitCode};

use clap::{Parser, Subcommand, ValueEnum};
use tracing::{error, level_filters::LevelFilter};
use tracing_subscriber::{layer::SubscriberExt, Layer};
use virtual_wall::{Result, StartOptions, VirtualWallManager};

#[derive(Clone, Debug, ValueEnum)]
enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

#[derive(Parser, Debug)]
#[command(name = "vw", about = "BroadcastXR Virtual Wall orchestrator CLI")]
struct Cli {
    /// Path to a virtual_wall.toml configuration file.
    #[arg(long)]
    config: Option<PathBuf>,

    /// Log level (overrides RUST_LOG).
    #[arg(long, default_value = "info", value_enum)]
    log_level: LogLevel,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Start or resume the Virtual Wall environment.
    Start {
        /// Number of bare-metal nodes to request.
        #[arg(long, default_value_t = 1)]
        nodes: usize,

        /// Number of logical paths to configure (currently informational).
        #[arg(long)]
        paths: Option<usize>,

        /// Reuse existing resources if available, true by default.
        #[arg(long, default_value_t = true)]
        reuse: bool,
    },
    /// Stop the environment and release all resources.
    Stop,
    // Recover from an existing environment.
    Recover,
    ExtendAll {
        #[arg(long)]
        duration: String,
    },
    Reset {
        /// Optional list of names/ids. If omitted, resets all resources.
        nodes: Vec<String>,
    },
    DownAll,
    Scp {
        src: String,
        dst: String,
    },
    /// Print the status of the current allocation.
    Status,
    /// List nodes in the environment.
    Nodes,
    /// List link metadata for the environment.
    Links,
    /// Execute a command on a node via SSH.
    Exec {
        /// Target node friendly name.
        node: String,

        /// Command to run (everything after `--` is treated as the command).
        #[arg(required = true, trailing_var_arg = true)]
        command: Vec<String>,

        /// SSH username override.
        #[arg(long)]
        username: Option<String>,

        /// Optional SSH identity file override.
        #[arg(long)]
        identity_file: Option<PathBuf>,
    },
    /// Extend the experiment lifetime.
    Extend {
        /// Duration accepted by the SLICES CLI (e.g., "4h", "1d").
        duration: String,
    },
    /// Execute ping between all nodes (from first node to others).
    PingAll,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    // Build the FmtSubscriber layer
    let fmt_layer = tracing_subscriber::fmt::layer()
        .pretty()
        .compact()
        .with_target(false)
        .with_file(true)
        .with_line_number(true)
        .with_thread_ids(true)
        .with_filter(match cli.log_level {
            LogLevel::Trace => LevelFilter::TRACE,
            LogLevel::Debug => LevelFilter::DEBUG,
            LogLevel::Info => LevelFilter::INFO,
            LogLevel::Warn => LevelFilter::WARN,
            LogLevel::Error => LevelFilter::ERROR,
        });

    let subscriber = { tracing_subscriber::registry().with(fmt_layer) };

    tracing::subscriber::set_global_default(subscriber)
        .expect("Failed to set global default subscriber");

    if let Err(err) = run(cli).await {
        error!("Error: {err}");
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

async fn run(cli: Cli) -> Result<()> {
    let config_path = cli.config.as_deref();
    let manager = VirtualWallManager::try_from_path(config_path)?;

    match cli.command {
        Commands::Start {
            nodes,
            paths,
            reuse,
        } => {
            let options = StartOptions {
                nodes,
                paths,
                reuse,
            };
            let summary = manager.start_from_options(options).await?;
            println!(
                "Experiment `{}` is ready with {} resources.",
                summary.experiment_name,
                summary.resources.len()
            );
            for resource in summary.resources {
                println!(
                    "- {} ({:?}) {:?}",
                    resource.name, resource.status, resource.addresses
                );
            }
        }
        Commands::Stop => {
            manager.disconnect().await?;
            println!("Disconnected from Virtual Wall resources.");
        }
        Commands::Recover => {
            let v = manager.recover().await?;
            println!("{}", serde_json::to_string_pretty(&v)?);
        }
        Commands::ExtendAll { duration } => {
            let v = manager.extend_all(&duration).await?;
            println!("{}", serde_json::to_string_pretty(&v)?);
        }
        Commands::Reset { nodes } => {
            let v = manager.reset(&nodes).await?;
            println!("{}", serde_json::to_string_pretty(&v)?);
        }
        Commands::DownAll => {
            manager.down_all().await?;
            println!("Destroyed all resources and cleared local state.");
        }
        Commands::Scp { src, dst } => {
            manager.scp(&src, &dst).await?;
            println!("SCP completed.");
        }
        Commands::Status => {
            let status = manager.status().await?;
            println!("{}", serde_json::to_string_pretty(&status)?);
        }
        Commands::Nodes => {
            let nodes = manager.nodes().await?;
            println!("{}", serde_json::to_string_pretty(&nodes)?);
        }
        Commands::Links => {
            let links = manager.links().await?;
            println!("{}", serde_json::to_string_pretty(&links)?);
        }
        Commands::Exec {
            node,
            command,
            username,
            identity_file,
        } => {
            let command_str = command.join(" ");
            let output = manager
                .exec(
                    &node,
                    &command_str,
                    username.as_deref(),
                    identity_file.as_deref(),
                    None,
                )
                .await?;
            println!("{output}");
        }
        Commands::Extend { duration } => {
            let summary = manager.extend_experiment(&duration).await?;
            println!("{}", serde_json::to_string_pretty(&summary)?);
        }
        Commands::PingAll => {
            let results = manager.ping_all().await?;
            println!("{}", serde_json::to_string_pretty(&results)?);
        }
    }

    Ok(())
}
