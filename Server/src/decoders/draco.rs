use draco_wrapper::decode_draco as draco_decode;
use tracing::{error, instrument, warn};

use shared_utils::types::{Point3D, PointCloudData};



#[instrument(skip_all)]
pub fn decode_draco(data: Vec<u8>) -> Result<PointCloudData, Box<dyn std::error::Error>> {
    match draco_decode(data) {
        Ok((vertices, colors)) => {
            // info!("Successfully decoded Draco data");
            // No errors, return 0 errors, along with decoded vertices and colors

            // Convert the vertices and colors into Point3D structs
            let mut points = Vec::with_capacity(vertices.len()/3);
            for i in (0..vertices.len()).step_by(3) {
                points.push(Point3D {
                    x: vertices[i],
                    y: vertices[i + 1],
                    z: vertices[i + 2],
                    r: colors[i],
                    g: colors[i + 1],
                    b: colors[i + 2],
                });
            }
            let pcd = PointCloudData {
                points,
                ..Default::default()
            };
            Ok(pcd)
        }
        Err(e) => {
            error!("Error decoding Draco data: {}", e);
            let pcd = PointCloudData {
                error_count: 1,
                ..Default::default()
            };
            // If there's an error, return 1 error and empty vectors
            Ok(pcd)
        }
    }
}