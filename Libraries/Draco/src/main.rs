//! Combine several folders full of Draco frames into a single stream.
//!
//! Each input directory is expected to contain files named like
//! `1.dra`, `2.dra`, … (any extension‐case is accepted).
//!
//! The tool decodes matching frames, concatenates their point clouds,
//! re-encodes the result, and writes it to the output directory with a
//! filename that embeds both an incremental **ID** (zero-padded so all
//! frames line up nicely) **and** the **total point count**:
//! e.g. `0001_123456.dra`, `0002_127999.dra`, …
//!
//! Translation / transform can be done as well

use std::{
    collections::HashMap, ffi::OsStr, fs, io, path::{Path, PathBuf}, time::Instant
};

use clap::Parser;
use nalgebra::{Rotation3, Vector3};
use serde::Deserialize;
use tracing::{error, info};
use tracing_subscriber::FmtSubscriber;

use draco_wrapper::*;

/// CLI arguments
#[derive(Parser, Debug)]
#[command(author, version, about = "Combine multiple Draco frame folders")]
struct Cli {
    /// One or more input directories that contain *.dra files.
    #[arg(value_name = "INPUT_DIR", required = true)]
    input_dirs: Vec<PathBuf>,

    /// Directory where combined frames will be written.
    #[arg(short, long, value_name = "OUT_DIR")]
    output: PathBuf,

    /// JSON file with per-directory transforms (optional).
    #[arg(short, long, value_name = "FILE")]
    config: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default)]
struct Transform {
    position: [f32; 3],
    rotation: [f32; 3], // Euler XYZ (rad)
    scale:    [f32; 3],
}
impl Default for Transform {
    fn default() -> Self {
        Self { position: [0.0; 3], rotation: [0.0; 3], scale: [1.0; 3] }
    }
}

/// Load `{ "dir1": { position:[…], rotation:[…], scale:[…] }, … }`
fn load_transforms(file: &Path) -> io::Result<HashMap<PathBuf, Transform>> {
    if !file.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("Transform config file {file:?} does not exist"),
        ));
    }

    if !file.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("Transform config path {file:?} is not a regular file"),
        ));
    }

    info!("Loading transform config from {:?}", file);

    let txt = fs::read_to_string(file)?;
    let raw = serde_json::from_str::<HashMap<String, Transform>>(&txt)
        .map_err(|e| {
            error!("Failed to parse JSON in {:?}: {}", file, e);
            io::Error::new(io::ErrorKind::InvalidData, e)
        })?;

    let mut result = HashMap::new();

    for (k, v) in raw {
        let pathbuf = PathBuf::from(&k);
        match pathbuf.canonicalize() {
            Ok(abs) => {
                info!("Transform: mapped {:?} → {:?} with {:?}", k, abs, v);
                result.insert(abs, v);
            }
            Err(e) => {
                error!(
                    "Could not canonicalize path {:?} from transform config: {}. Using as-is.",
                    k, e
                );
                result.insert(pathbuf.clone(), v);
            }
        }
    }

    Ok(result)
}

/// Apply T * R * S in place (scale → rotate → translate).
fn transform_vertices(v: &mut [f32], t: &Transform) {
    if t == &Transform::default() { return; }

    let rot = Rotation3::<f32>::from_euler_angles(t.rotation[0], t.rotation[1], t.rotation[2]);
    let trn = Vector3::from(t.position);
    let scl = Vector3::from(t.scale);

    for chunk in v.chunks_exact_mut(3) {
        let p = Vector3::new(chunk[0], chunk[1], chunk[2]).component_mul(&scl);
        let p = rot * p + trn;
        chunk[0] = p.x; chunk[1] = p.y; chunk[2] = p.z;
    }
}

/// Discover and lexicographically sort every “*.dra” file in `dir`.
fn discover_frames(dir: &Path) -> io::Result<Vec<PathBuf>> {
    info!("Discovering frames in {:?}", dir);
    let mut frames: Vec<_> = fs::read_dir(dir)?
        .filter_map(|entry| {
            entry.ok().and_then(|e| {
                let path = e.path();
                if path
                    .extension()
                    .and_then(OsStr::to_str)
                    .map_or(false, |ext| ext.eq_ignore_ascii_case("dra"))
                {
                    Some(path)
                } else {
                    None
                }
            })
        })
        .collect();

    frames.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
    Ok(frames)
}

/// Compute the number of decimal digits required to pad `n`.
fn digits(n: usize) -> usize {
    let n = n.max(1) as f64;
    n.log10().floor() as usize + 1
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // ---------------------------------------------------------------------
    // 0. Initialisation
    // ---------------------------------------------------------------------
    let subscriber = FmtSubscriber::new();
    tracing::subscriber::set_global_default(subscriber)?;

    let cli = Cli::parse();

    // Print the current working directory
    info!("Current working directory: {:?}", std::env::current_dir());

    // -------------------------------------------------------------------------
    // Ensure output directory exists, or create it
    // -------------------------------------------------------------------------
    info!("Output directory: {:?}", cli.output);
    if !cli.output.exists() {
        fs::create_dir_all(&cli.output)?;
    } else if !cli.output.is_dir() {
        return Err(format!("Output path {:?} is not a directory", cli.output).into());
    }

    // -------------------------------------------------------------------------
    // Load per-directory transforms (if any)
    // -------------------------------------------------------------------------
    let transforms = if let Some(cfg) = &cli.config {
        load_transforms(cfg)?
    } else {
        HashMap::new()
    };

    // Make sure we work with canonical / absolute paths so that look-ups match
    let input_dirs: Vec<PathBuf> = cli
        .input_dirs
        .iter()
        .map(|p| p.canonicalize().unwrap_or_else(|_| p.clone()))
        .collect();

    // ---------------------------------------------------------------------
    // 1. Index all frames in every input directory
    // ---------------------------------------------------------------------
    let mut dir_frames: Vec<Vec<PathBuf>> = Vec::with_capacity(input_dirs.len());
    let mut max_frames = 0usize;

    for dir in &input_dirs {
        let mut frames = discover_frames(dir)?;
        max_frames = max_frames.max(frames.len());
        info!("Found {} frame(s) in {:?}", frames.len(), dir);
        dir_frames.push(std::mem::take(&mut frames));
    }

    if max_frames == 0 {
        return Err("No Draco files found in any input directory".into());
    }

    let pad_width = digits(max_frames);
    info!(
        "Will output {} combined frame(s) (ID padding width = {})",
        max_frames, pad_width
    );

    let start_time = Instant::now();

    // ---------------------------------------------------------------------
    // 2. Process each frame index
    // ---------------------------------------------------------------------
    for (zero_idx, id) in (1..=max_frames).enumerate() {
        let frame_start = Instant::now();

        let mut combined_vertices = Vec::<f32>::new();
        let mut combined_colors = Vec::<u8>::new();

        // -----------------------------------------------------------------
        // 2a. Collect matching frames across all input dirs
        // -----------------------------------------------------------------
        for (dir_idx, dir) in input_dirs.iter().enumerate() {
            if let Some(path) = dir_frames[dir_idx].get(zero_idx) {
                let data = fs::read(path)?;
                match decode_draco(data) {
                    Ok((mut verts, cols)) => {
                        // Apply (per-folder) transform before concatenation
                        let default_tf = Transform::default();
                        let tf = transforms.get(dir).unwrap_or(&default_tf);
                        transform_vertices(&mut verts, tf);
                        combined_vertices.extend_from_slice(&verts);
                        combined_colors.extend_from_slice(&cols);
                    }
                    Err(e) => error!("Failed to decode {:?}: {e}", path),
                }
            }
        }

        let point_count = (combined_vertices.len() / 3) as u64;
        if point_count == 0 {
            error!("Combined frame #{id} contained no points - skipping");
            continue;
        }

        // -----------------------------------------------------------------
        // 2b. Re-encode everything as a single Draco chunk
        // -----------------------------------------------------------------
        let encode_start = Instant::now();
        let encoded = encode_draco(combined_vertices, combined_colors)?;
        info!(
            "  📦 Encoded {:>pad$} → {} bytes in {:.2?}",
            id,
            encoded.len(),
            encode_start.elapsed(),
            pad = pad_width
        );

        // -----------------------------------------------------------------
        // 2c. Write it out
        // -----------------------------------------------------------------
        let outfile = cli.output.join(format!("{id:0pad_width$}_{point_count}.dra"));

        fs::write(&outfile, &encoded)?;
        info!(
            "  💾 Wrote frame {:>pad$} → {:?} ({} pts) in {:.2?}",
            id,
            outfile,
            point_count,
            frame_start.elapsed(),
            pad = pad_width
        );
    }

    info!("🏁 Finished all frames in {:.2?}", start_time.elapsed());

    Ok(())
}
