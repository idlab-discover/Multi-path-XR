// File: args.rs
use clap::{Parser, ValueEnum};
use tracing::level_filters::LevelFilter;

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug, ValueEnum)]
pub enum LogLevel {
    Trace = 0, // Designates very fine-grained informational events, extremely verbose.
    Debug = 1, // Designates fine-grained informational events.
    Info = 2, // Designates informational messages.
    Warn = 3, // Designates hazardous situations.
    Error = 4, // Designates very serious errors.
}

#[derive(Parser, Debug)]
#[command(version, about, long_about="A Headless client that receives 3D data from a server.")]
pub struct Args {
    #[arg(short, long, default_value = "http://localhost:3001")]
    pub server_url: String,
    #[arg(short, long, default_value = "udp://239.0.2.1:40085")]
    pub multicast_url: String,
    #[arg(short, long, action = clap::ArgAction::SetTrue)]
    pub disable_parser: bool,
    #[arg(short, long, default_value = "info")]
    pub log_level: LogLevel,
    #[arg(short, long, default_value = "3380")]
    pub port: u16,
}

pub fn parse_args() -> Args {
    Args::parse()
}

pub fn get_log_level_filter(args: &Args) -> LevelFilter {
    // Map the LogLevel enum to the LevelFilter enum
    match args.log_level {
        LogLevel::Trace => LevelFilter::TRACE,
        LogLevel::Debug => LevelFilter::DEBUG,
        LogLevel::Info => LevelFilter::INFO,
        LogLevel::Warn => LevelFilter::WARN,
        LogLevel::Error => LevelFilter::ERROR,
    }
}
