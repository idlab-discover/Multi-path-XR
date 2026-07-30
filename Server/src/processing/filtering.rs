//use tracing::instrument;

// processing/filtering.rs
use crate::types::Fov;

use spatial_utils::point::Point3D;

#[allow(dead_code)]
//#[instrument(skip_all)]
pub fn filter_by_fov(points: &[Point3D], _fov: &Fov) -> Vec<Point3D> {
    // Implement the filtering logic based on FOV
    points
        .iter()
        .filter(|_point| {
            // Check if the point is within the FOV
            true // Placeholder
        })
        .cloned()
        .collect()
}
