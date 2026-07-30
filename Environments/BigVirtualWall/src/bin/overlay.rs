use std::{
    fs,
    path::{Path, PathBuf},
};

use std::collections::HashMap;

use big_virtual_wall::{
    apply_overlay, clean_overlay, discover_hosts, generate_host_scripts,
    plan_overlay_with_underlay, validate_safety, OverlaySpec,
};
use clap::{Parser, Subcommand};
use virtual_wall::{Result, VirtualWallManager};

#[derive(Parser, Debug)]
#[command(name = "overlay", about = "Big Virtual Wall overlay tool")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Plan overlay and emit per-host setup scripts.
    Plan {
        /// Overlay spec file (yaml/json/toml).
        #[arg(long)]
        spec: PathBuf,
        /// Output directory for host scripts (default: print to stdout).
        #[arg(long)]
        out_dir: Option<PathBuf>,
        /// Optional underlay map (json/yaml/toml) of {host: underlay_ip} when spec omits underlay.
        #[arg(long)]
        underlay_map: Option<PathBuf>,
        /// Discover hosts/underlay directly from an experiment using VirtualWall.
        #[arg(long)]
        experiment: Option<String>,

        /// Only print scripts, do not apply.
        #[arg(long, default_value_t = false)]
        dry_run: bool,
    },
    /// Apply overlay: generate and execute scripts on target hosts.
    Apply {
        /// Overlay spec file (yaml/json/toml).
        #[arg(long)]
        spec: PathBuf,
        /// Optional underlay map (json/yaml/toml) of {host: underlay_ip} when spec omits underlay.
        #[arg(long)]
        underlay_map: Option<PathBuf>,
        /// Discover hosts/underlay directly from an experiment using VirtualWall.
        #[arg(long)]
        experiment: Option<String>,
        /// Dry run: print scripts instead of executing.
        #[arg(long, default_value_t = false)]
        dry_run: bool,
    },
    /// Clean overlay artifacts from hosts.
    Clean {
        /// Overlay spec file (yaml/json/toml).
        #[arg(long)]
        spec: PathBuf,
        /// Optional underlay map (json/yaml/toml) of {host: underlay_ip} when spec omits underlay.
        #[arg(long)]
        underlay_map: Option<PathBuf>,
        /// Discover hosts/underlay directly from an experiment using VirtualWall.
        #[arg(long)]
        experiment: Option<String>,
        /// Dry run: print cleanup scripts instead of executing.
        #[arg(long, default_value_t = false)]
        dry_run: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Plan {
            spec,
            out_dir,
            underlay_map,
            experiment,
            dry_run,
        } => {
            run_plan(
                &spec,
                out_dir.as_deref(),
                underlay_map.as_deref(),
                experiment,
                dry_run,
            )
            .await?;
        }
        Commands::Apply {
            spec,
            underlay_map,
            experiment,
            dry_run,
        } => {
            run_apply(&spec, underlay_map.as_deref(), experiment, dry_run).await?;
        }
        Commands::Clean {
            spec,
            underlay_map,
            experiment,
            dry_run,
        } => {
            run_clean(&spec, underlay_map.as_deref(), experiment, dry_run).await?;
        }
    }
    Ok(())
}

async fn run_plan(
    spec_path: &Path,
    out_dir: Option<&Path>,
    map_path: Option<&Path>,
    experiment: Option<String>,
    dry_run: bool,
) -> Result<()> {
    // Surface experiment early so the VirtualWall manager picks it up even when we
    // don't perform discovery (e.g., when a map file is provided).
    if let Some(exp) = &experiment {
        std::env::set_var("SLICES_EXPERIMENT", exp);
    }

    let spec = OverlaySpec::load(spec_path)?;
    let underlay_map = match experiment {
        Some(_) => {
            let mgr = VirtualWallManager::try_from_path(None)?;
            let inv = discover_hosts(&mgr).await?;
            let mut map = HashMap::new();
            for h in inv {
                map.insert(h.name, h.underlay);
            }
            map
        }
        None => load_underlay_map(map_path)?,
    };
    validate_safety(&spec, &underlay_map)?;
    let plan = plan_overlay_with_underlay(&spec, &underlay_map);
    let scripts = generate_host_scripts(&plan, spec.vlan_bindings.as_ref());

    if dry_run {
        for (host, script) in &scripts {
            println!("### HOST: {host}\n{script}");
        }
        return Ok(());
    }

    if let Some(dir) = out_dir {
        fs::create_dir_all(dir)?;
        for (host, script) in scripts {
            let path = dir.join(format!("{host}.sh"));
            fs::write(&path, script)?;
            println!("Wrote {}", path.display());
        }
    } else {
        for (host, script) in scripts {
            println!("### HOST: {host}\n{script}");
        }
    }
    Ok(())
}

fn load_underlay_map(map_path: Option<&Path>) -> Result<HashMap<String, String>> {
    if let Some(p) = map_path {
        let contents = fs::read_to_string(p)?;
        let ext = p.extension().and_then(|s| s.to_str()).unwrap_or("json");
        let map: HashMap<String, String> = match ext {
            "yaml" | "yml" => serde_yaml::from_str(&contents)?,
            "toml" => toml::from_str(&contents)?,
            _ => serde_json::from_str(&contents)?,
        };
        return Ok(map);
    }
    Ok(HashMap::new())
}

async fn run_apply(
    spec_path: &Path,
    map_path: Option<&Path>,
    experiment: Option<String>,
    dry_run: bool,
) -> Result<()> {
    if let Some(exp) = &experiment {
        std::env::set_var("SLICES_EXPERIMENT", exp);
    }

    let spec = OverlaySpec::load(spec_path)?;
    let mgr = VirtualWallManager::try_from_path(None)?;

    // Choose underlay mapping: priority experiment discovery > provided map > manager discovery in apply_overlay.
    let underlay_map = match experiment {
        Some(_) => {
            let inv = discover_hosts(&mgr).await?;
            let mut map = HashMap::new();
            for h in inv {
                map.insert(h.name, h.underlay);
            }
            Some(map)
        }
        None => {
            if map_path.is_some() {
                Some(load_underlay_map(map_path)?)
            } else {
                None
            }
        }
    };

    if let Some(ref map) = underlay_map {
        validate_safety(&spec, map)?;
    }

    apply_overlay(&mgr, &spec, underlay_map, dry_run).await?;
    Ok(())
}

async fn run_clean(
    spec_path: &Path,
    map_path: Option<&Path>,
    experiment: Option<String>,
    dry_run: bool,
) -> Result<()> {
    if let Some(exp) = &experiment {
        std::env::set_var("SLICES_EXPERIMENT", exp);
    }

    let spec = OverlaySpec::load(spec_path)?;
    let mgr = VirtualWallManager::try_from_path(None)?;

    let underlay_map = match experiment {
        Some(_) => {
            let inv = discover_hosts(&mgr).await?;
            let mut map = HashMap::new();
            for h in inv {
                map.insert(h.name, h.underlay);
            }
            Some(map)
        }
        None => load_underlay_map(map_path).ok().filter(|m| !m.is_empty()),
    };

    if let Some(ref map) = underlay_map {
        validate_safety(&spec, map)?;
    }

    clean_overlay(&mgr, &spec, underlay_map, dry_run).await?;
    Ok(())
}
