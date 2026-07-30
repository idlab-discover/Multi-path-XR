// ./Libraries/pcf/src/bin/pcf_bench.rs
use clap::{Parser, ValueEnum};
use pcf::bench::{config::BenchConfig, runner::run};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Mode {
    Accuracy = 0,
    Throughput = 1,
}

impl std::fmt::Display for Mode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Mode::Accuracy => write!(f, "accuracy"),
            Mode::Throughput => write!(f, "throughput"),
        }
    }
}

#[derive(Debug, Parser)]
#[command(name = "pcf_bench", version)]
struct Cli {
    /// Path to bench config (TOML)
    #[arg(long)]
    config: PathBuf,
    /// Output CSV path
    #[arg(long)]
    out: PathBuf,
    /// Mode: accuracy (sequential) or throughput (parallel)
    #[arg(long, default_value_t=Mode::Accuracy)]
    mode: Mode,
    /// Max parallel jobs (only used in throughput mode). 0 = num_cpus
    #[arg(long, default_value_t = 0)]
    jobs: usize,
    /// Disable progress bars (useful for CI logs)
    #[arg(long, default_value_t = false)]
    no_progress: bool,
    /// Warm-up encode/decode before timing
    #[arg(long, default_value_t = true)]
    warmup: bool,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let cfg_text = std::fs::read_to_string(&cli.config)?;
    let mut cfg: BenchConfig = toml::from_str(&cfg_text)?;

    // Drive a couple of runner flags from CLI (optional)
    cfg.mode = match cli.mode {
        Mode::Accuracy => pcf::bench::config::Mode::Accuracy,
        Mode::Throughput => pcf::bench::config::Mode::Throughput,
    };
    cfg.warmup = cli.warmup;
    cfg.progress = !cli.no_progress;

    run(cfg, Some(cli.out.to_str().unwrap()))
}
