use draco_wrapper::encode_draco as DW_encode;
use tracing::instrument;

use shared_utils::types::PointCloudData;

#[instrument(skip_all)]
pub fn encode_draco(point_cloud: PointCloudData) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    // Use draco compression library

    // Convert PointCloudData to Vec<f32> and Vec<u8>
    let mut vertices = Vec::with_capacity(point_cloud.points.len() * 3);
    let mut colors_rgb = Vec::with_capacity(point_cloud.points.len() * 3);

    for point in point_cloud.points.iter() {
        vertices.push(point.x);
        vertices.push(point.y);
        vertices.push(point.z);

        colors_rgb.push(point.r);
        colors_rgb.push(point.g);
        colors_rgb.push(point.b);
    }

    let compressed_data = DW_encode(vertices, colors_rgb)?;

    Ok(compressed_data)
}

