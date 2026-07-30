use clap::{Parser, ValueEnum};
use tracing::level_filters::LevelFilter;

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug, ValueEnum)]
pub enum LogLevel {
    Trace = 0,
    Debug = 1,
    Info = 2,
    Warn = 3,
    Error = 4,
}

impl LogLevel {
    pub fn as_level_filter(self) -> LevelFilter {
        match self {
            Self::Trace => LevelFilter::TRACE,
            Self::Debug => LevelFilter::DEBUG,
            Self::Info => LevelFilter::INFO,
            Self::Warn => LevelFilter::WARN,
            Self::Error => LevelFilter::ERROR,
        }
    }
}

#[derive(Parser, Debug)]
#[command(author, version, about = "pc-agent")]
pub struct Args {
    /// The URL of the controller to connect to (e.g., http://localhost:3000)
    #[clap(short, long, default_value = "http://localhost:3000")]
    pub url: String,
    /// The node id of the agent (e.g., n1)
    #[clap(short, long, default_value = "n0")]
    pub node_id: String,
    /// Set the log level (possible values: error, warn, info, debug, trace)
    #[arg(short, long, default_value = "info")]
    pub log_level: LogLevel,
}
