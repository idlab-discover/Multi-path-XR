// generator/mod.rs

use rayon::iter::{IntoParallelIterator, ParallelIterator};
use serde::{Deserialize, Serialize};
use tracing::instrument;

use shared_utils::types::{Point3D, PointCloudData};
use std::time::{SystemTime, UNIX_EPOCH};

// For vector/quaternion math
use glam::{EulerRot, Quat, Vec3A};

// Provide an enum to represent the different generators
#[derive(Debug, Serialize, Deserialize, PartialEq, Copy, Clone)]
pub enum GeneratorName {
    Basic = 0,
    Cube,
}

/// Placeholder function to generate a point cloud
#[instrument(skip_all)]
pub fn generate_basic_point_cloud() -> PointCloudData {
    // Generate some dummy points
    let points = vec![
        Point3D {
            x: 0.0,
            y: 0.0,
            z: 0.0,
            r: 255,
            g: 255,
            b: 255,
        },
        // A point on the x-axis
        Point3D {
            x: 1.0,
            y: 0.0,
            z: 0.0,
            r: 255,
            g: 0,
            b: 0,
        },
        // A point on the y-axis
        Point3D {
            x: 0.0,
            y: 1.0,
            z: 0.0,
            r: 0,
            g: 255,
            b: 0,
        },
        // A point on the z-axis
        Point3D {
            x: 0.0,
            y: 0.0,
            z: 1.0,
            r: 0,
            g: 0,
            b: 255,
        },
        // Add more points as needed
    ];

    // Get the current time
    let since_the_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards");
    let current_time_us = since_the_epoch.as_micros() as u64;

    PointCloudData {
        points,
        creation_time: current_time_us,
        presentation_time: current_time_us,
        error_count: 0,
    }
}

/// Convert an HSV color (h ∈ [0, 360), s ∈ [0, 1], v ∈ [0, 1]) 
/// to an RGB tuple (r, g, b) ∈ [0, 1].
fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (f32, f32, f32) {
    let h = h % 360.0;
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;

    let (r_prime, g_prime, b_prime) = match (h / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };

    (r_prime + m, g_prime + m, b_prime + m)
}

/// Clamps a floating-point color value [0.0, 1.0] to [0, 255].
fn float_to_u8_channel(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0) as u8
}

/// Generates a cube of points with:
/// - Time-based hue for coloring,
/// - Time-based rotation on all axes,
/// - Lighting/shading from a given light direction.
#[instrument(skip_all)]
pub fn generate_shaded_cube_point_cloud(
    cube_size: usize, // e.g., 10
    cube_point_spacing: f32, // e.g., 1.0
    light_direction: [f32; 3],// e.g., [1.0, 1.0, 1.0]
    rotation_speed_degs_per_sec: f32,   // Degrees per second
) -> PointCloudData {
    // 1) Current timestamp for hue and rotation
    let since_the_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards");
    let current_time_us = since_the_epoch.as_micros() as u64;
    let current_time_ms = current_time_us.saturating_div(1000);

    // 2) Time-based hue: mimic (DateTime.Now.TimeOfDay.TotalSeconds * 60) % 360
    let hue = (current_time_ms as f32 * 60.0) % 360.0;
    let (base_r, base_g, base_b) = hsv_to_rgb(hue, 1.0, 1.0);

    // 3) Rotation angle in degrees -> convert to radians when creating the Quat
    let rotation_angle_degs = current_time_ms as f32 * rotation_speed_degs_per_sec;
    let rotation_angle_rads = rotation_angle_degs.to_radians();
    let rotation = Quat::from_euler(
        EulerRot::XYZ,
        rotation_angle_rads,
        rotation_angle_rads,
        rotation_angle_rads,
    );

    // 4) Center the cube at half-size
    //    Example: If cube_size=10, and spacing=1.0, center = (5.0, 5.0, 5.0)
    //    so that i * spacing - center shifts the cube around origin.
    let half_size = cube_size as f32 / 2.0;
    let center = Vec3A::new(
        half_size * cube_point_spacing,
        half_size * cube_point_spacing,
        half_size * cube_point_spacing,
    );

    // 5) Normalize the input light direction once
    let light_direction = Vec3A::new(light_direction[0], light_direction[1], light_direction[2]);
    let light_dir_normalized = light_direction.normalize();

    // 6) Generate points in parallel
    let points: Vec<Point3D> = (0..cube_size)
        .into_par_iter()
        .flat_map_iter(|i| {
            // 7) Generate one "slice" of the cube
            (0..cube_size).flat_map(move |j| {
                // 8) Generate one row of the slice
                (0..cube_size).map(move |k: usize| {
                    // (a) Original point in "cube space"
                    let px = i as f32 * cube_point_spacing;
                    let py = j as f32 * cube_point_spacing;
                    let pz = k as f32 * cube_point_spacing;
                    let p = Vec3A::new(px, py, pz);

                    // (b) Shift so that the cube is centered around (0,0,0),
                    //     then rotate, then shift back
                    let point_centered = p - center;
                    let rotated_point_centered = rotation * point_centered;
                    let final_point = rotated_point_centered + center;

                    // (c) Approximate per-vertex normal for shading
                    //     The logic is: 
                    //       distance to center in each dimension ∈ [0,1],
                    //       direction left/right, up/down, back/front.
                    let distance_to_center_x = ((i as f32) - half_size).abs() / half_size;
                    let distance_to_center_y = ((j as f32) - half_size).abs() / half_size;
                    let distance_to_center_z = ((k as f32) - half_size).abs() / half_size;

                    let mut normal = Vec3A::ZERO;
                    // Left or right side
                    if i as f32 <= half_size {
                        normal += Vec3A::new(-1.0, 0.0, 0.0) * distance_to_center_x;
                    } else {
                        normal += Vec3A::new(1.0, 0.0, 0.0) * distance_to_center_x;
                    }
                    // Bottom or top
                    if j as f32 <= half_size {
                        normal += Vec3A::new(0.0, -1.0, 0.0) * distance_to_center_y;
                    } else {
                        normal += Vec3A::new(0.0, 1.0, 0.0) * distance_to_center_y;
                    }
                    // Back or front
                    if k as f32 <= half_size {
                        normal += Vec3A::new(0.0, 0.0, -1.0) * distance_to_center_z;
                    } else {
                        normal += Vec3A::new(0.0, 0.0, 1.0) * distance_to_center_z;
                    }

                    // Rotate the normal as well
                    let rotated_normal = rotation * normal;
                    let normalized_normal = rotated_normal.normalize_or_zero();

                    // (d) Compute diffuse intensity = dot(normal, light_dir)
                    let mut intensity = normalized_normal.dot(light_dir_normalized).clamp(0.0, 1.0);

                    // (e) Remap intensity so we never go fully dark 
                    //     (similar to a "minimum ambient" in 3D)
                    //     e.g., 0.2..1.0 range
                    intensity = 0.2 + intensity * 0.8; 

                    // (f) Multiply base color by intensity
                    let shaded_r = float_to_u8_channel(base_r * intensity);
                    let shaded_g = float_to_u8_channel(base_g * intensity);
                    let shaded_b = float_to_u8_channel(base_b * intensity);

                    // (g) Store final point in our custom type
                    Point3D {
                        x: final_point.x,
                        y: final_point.y,
                        z: final_point.z,
                        r: shaded_r,
                        g: shaded_g,
                        b: shaded_b,
                    }
                })
            })
        })
        .collect();

    // Wrap up in your PointCloudData
    PointCloudData {
        points,
        creation_time: current_time_us,
        presentation_time: current_time_us,
        error_count: 0,
    }
}
