use nalgebra::{Matrix3, Rotation3, SymmetricEigen, UnitQuaternion, Vector3};
use shared_utils::types::{SpatialFrameData, SpatialPayload};
use spatial_utils::sampling::exact_random::exact_random_sampling;
use spatial_utils::splat::GaussianSplatF32;

use crate::types::StreamSettings;

/// Apply the same steps the aggregator would have done - but on a single frame.
pub fn prep_for_encoding(
    mut frame: SpatialFrameData,
    settings: &StreamSettings,
    max_primitives: Option<u64>,
) -> SpatialFrameData {
    // Apply offset and rotation
    let position = settings.position;
    let rotation = settings.rotation; // Create scale vector
    let scale = settings.scale;

    // If all the above values are zero, then skip the transformation
    if position != [0.0, 0.0, 0.0] || rotation != [0.0, 0.0, 0.0] || scale != [1.0, 1.0, 1.0] {
        // Create rotation matrix
        let rotation_matrix = Rotation3::from_euler_angles(rotation[0], rotation[1], rotation[2]);

        // Create translation vector
        let translation = Vector3::new(position[0], position[1], position[2]);
        let scale_matrix = Matrix3::from_diagonal(&Vector3::new(scale[0], scale[1], scale[2]));
        let linear_transform = rotation_matrix.matrix() * scale_matrix;

        match &mut frame.payload {
            SpatialPayload::Points(points) => {
                for point in points {
                    let scaled_point =
                        Vector3::new(point.x * scale[0], point.y * scale[1], point.z * scale[2]);

                    // Apply rotation and translation
                    let transformed_point = rotation_matrix * scaled_point + translation;

                    // Overwrite the original point with the transformed point
                    point.x = transformed_point.x;
                    point.y = transformed_point.y;
                    point.z = transformed_point.z;
                }
            }
            SpatialPayload::GaussianSplats(splats) => {
                for splat in splats {
                    transform_splat(splat, &linear_transform, translation);
                }
            }
        }
    }

    // 2) optional down‑sampling  -------------------------------------------
    if let Some(limit) = max_primitives {
        match &mut frame.payload {
            SpatialPayload::Points(points) if points.len() as u64 > limit => {
                *points = exact_random_sampling(points, limit as usize);
            }
            SpatialPayload::GaussianSplats(splats) if splats.len() as u64 > limit => {
                *splats = exact_random_sampling(splats, limit as usize);
            }
            _ => {}
        }
    }
    frame
}

fn transform_splat(
    splat: &mut GaussianSplatF32,
    linear_transform: &Matrix3<f32>,
    translation: Vector3<f32>,
) {
    let mean = Vector3::new(splat.mean[0], splat.mean[1], splat.mean[2]);
    let transformed_mean = linear_transform * mean + translation;
    splat.mean = [transformed_mean.x, transformed_mean.y, transformed_mean.z];

    let rotation = rotation_matrix_from_quat_wxyz(splat.rotation);
    let scale = Vector3::new(
        splat.scale[0].max(0.0),
        splat.scale[1].max(0.0),
        splat.scale[2].max(0.0),
    );
    let scale_covariance = Matrix3::from_diagonal(&Vector3::new(
        scale.x * scale.x,
        scale.y * scale.y,
        scale.z * scale.z,
    ));
    let covariance = rotation * scale_covariance * rotation.transpose();
    let transformed_covariance = linear_transform * covariance * linear_transform.transpose();
    let decomposition = SymmetricEigen::new(transformed_covariance);

    let mut axes = [
        (
            decomposition.eigenvalues[0].max(0.0),
            decomposition.eigenvectors.column(0).into_owned(),
        ),
        (
            decomposition.eigenvalues[1].max(0.0),
            decomposition.eigenvectors.column(1).into_owned(),
        ),
        (
            decomposition.eigenvalues[2].max(0.0),
            decomposition.eigenvectors.column(2).into_owned(),
        ),
    ];
    axes.sort_by(|left, right| right.0.total_cmp(&left.0));

    let mut rotation_basis = Matrix3::from_columns(&[axes[0].1, axes[1].1, axes[2].1]);
    if rotation_basis.determinant() < 0.0 {
        rotation_basis.set_column(2, &(-rotation_basis.column(2)));
    }

    let rotation = Rotation3::from_matrix_unchecked(rotation_basis);
    let quaternion = UnitQuaternion::from_rotation_matrix(&rotation);
    let quaternion = quaternion.quaternion();
    splat.rotation = [quaternion.w, quaternion.i, quaternion.j, quaternion.k];
    splat.scale = [axes[0].0.sqrt(), axes[1].0.sqrt(), axes[2].0.sqrt()];
}

fn rotation_matrix_from_quat_wxyz(quaternion: [f32; 4]) -> Matrix3<f32> {
    let norm = (quaternion[0] * quaternion[0]
        + quaternion[1] * quaternion[1]
        + quaternion[2] * quaternion[2]
        + quaternion[3] * quaternion[3])
        .sqrt();
    if norm.is_finite() && norm > 0.0 {
        let normalized = nalgebra::Quaternion::new(
            quaternion[0] / norm,
            quaternion[1] / norm,
            quaternion[2] / norm,
            quaternion[3] / norm,
        );
        UnitQuaternion::from_quaternion(normalized)
            .to_rotation_matrix()
            .into_inner()
    } else {
        Matrix3::identity()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::StreamSettings;
    use shared_utils::types::{SpatialFrameData, SpatialPayload};
    use spatial_utils::color::Rgba8;

    fn default_settings() -> StreamSettings {
        StreamSettings {
            stream_id: "test".to_owned(),
            priority: 0,
            egress_protocols: Vec::new(),
            process_incoming_frames: true,
            position: [0.0, 0.0, 0.0],
            rotation: [0.0, 0.0, 0.0],
            scale: [1.0, 1.0, 1.0],
            presentation_time_offset: None,
            client_id: None,
            quality_index: None,
            input_format: Default::default(),
            decode_bypass: false,
            aggregator_bypass: false,
            ring_buffer_bypass: false,
            max_primitive_percentages: None,
        }
    }

    #[test]
    fn non_uniform_stream_scale_updates_splat_axes_without_average_scale() {
        let mut settings = default_settings();
        settings.scale = [2.0, 3.0, 4.0];
        let frame = SpatialFrameData {
            payload: SpatialPayload::GaussianSplats(vec![GaussianSplatF32::new(
                [1.0, 1.0, 1.0],
                Rgba8::new(255, 255, 255, 255),
                [1.0, 1.0, 1.0],
                [1.0, 0.0, 0.0, 0.0],
            )]),
            creation_time: 0,
            presentation_time: 0,
            error_count: 0,
        };

        let transformed = prep_for_encoding(frame, &settings, None);

        let SpatialPayload::GaussianSplats(splats) = transformed.payload else {
            panic!("expected Gaussian splat payload");
        };
        let splat = &splats[0];
        assert_eq!(splat.mean, [2.0, 3.0, 4.0]);
        assert_eq!(splat.scale, [4.0, 3.0, 2.0]);
    }
}
