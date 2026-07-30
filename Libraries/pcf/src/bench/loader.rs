use spatial_codecs::decoder::decode_to_points_vec;
use spatial_utils::point::Point3D;
use std::path::{Path, PathBuf};

pub fn discover_ply(
    dir: &Path,
    limit: Option<usize>,
) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    let mut v: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|e| e.to_string_lossy().to_lowercase()) == Some("ply".into()))
        .collect();
    v.sort();
    if let Some(n) = limit {
        v.truncate(n);
    }
    Ok(v)
}

pub fn load_points(path: &Path) -> Result<Vec<Point3D>, Box<dyn std::error::Error>> {
    let bytes = std::fs::read(path)?;
    let pts = decode_to_points_vec(&bytes)?;
    Ok(pts)
}
