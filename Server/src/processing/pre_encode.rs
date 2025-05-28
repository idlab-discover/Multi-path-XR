use nalgebra::{Vector3, Rotation3};
use shared_utils::types::PointCloudData;

use crate::{processing::sampling::exact_random_sampling, types::StreamSettings};

/// Apply the same steps the aggregator would have done – but on a single cloud.
pub fn prep_for_encoding(
    mut pc: PointCloudData,
    settings: &StreamSettings,
    max_points: Option<u64>,
) -> PointCloudData {
    // Apply offset and rotation
    let position = settings.position;
    let rotation = settings.rotation;            // Create scale vector
    let scale = settings.scale;

    // If all the above values are zero, then skip the transformation
    if position != [0.0, 0.0, 0.0] || rotation != [0.0, 0.0, 0.0] || scale != [1.0, 1.0, 1.0] {

        // Create rotation matrix
        let rotation_matrix = Rotation3::from_euler_angles(
            rotation[0],
            rotation[1],
            rotation[2],
        );

        // Create translation vector
        let translation = Vector3::new(position[0], position[1], position[2]);

        // Expand the capacity of the combined_points vector

        for point in &mut pc.points {
            let scaled_point = Vector3::new(point.x * scale[0], point.y * scale[1], point.z * scale[2]);

            // Apply rotation and translation
            let transformed_point = rotation_matrix * scaled_point + translation;

            // Overwrite the original point with the transformed point
            point.x = transformed_point.x;
            point.y = transformed_point.y;
            point.z = transformed_point.z;
        }
    }

    // 2) optional down‑sampling  -------------------------------------------
    if let Some(limit) = max_points {
        if pc.points.len() as u64 > limit {
            pc.points = exact_random_sampling(&pc.points, limit as usize);
        }
    }
    pc
}
