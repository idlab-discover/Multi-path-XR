use tracing::instrument;
use tmf::{FloatType, TMFMesh, TMFPrecisionInfo};

use shared_utils::types::PointCloudData;

#[instrument(skip_all)]
pub fn encode_tmf(point_cloud: PointCloudData) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    // Convert your PointCloudData into TMFMesh
    let mut mesh = TMFMesh::empty();

    // Pre-allocate capacity
    let n = point_cloud.points.len();
    let mut vertices = Vec::with_capacity(n);
    let mut color_ints = Vec::with_capacity(n);

    // Convert each point into a Vector3 for the mesh and a u32 for the color
    for p in &point_cloud.points {
        // Positions go in mesh vertices
        vertices.push((p.x as FloatType, p.y as FloatType, p.z as FloatType));

        // Pack (r,g,b) = 0x00RRGGBB
        // using r<<16 | g<<8 | b. 
        // That becomes one integer per point in the custom data array.
        let packed_color = ((p.r as u32) << 16)
                         | ((p.g as u32) <<  8)
                         |  (p.b as u32);
        color_ints.push(packed_color);
    }
    
    // Assign them to the mesh
    mesh.set_vertices(vertices);
    
    // Add the RGBA array as custom data
    // The name "point_colors" can be any nonempty string
    mesh.add_custom_data(color_ints[..].into(), "colors").expect("Could not add custom data to mesh!");
    
    // Finally, encode to a buffer:
    let mut buffer = Vec::with_capacity(n * 16); // pre-allocate some guess
    let precision_info = TMFPrecisionInfo::default(); 
    // For a pure point cloud, the “shortest_edge” logic in tmf is not that critical,
    // but we still call `write_tmf_one(...)` with a name
    mesh.write_tmf_one(&mut buffer, &precision_info, "pc")?;

    Ok(buffer)


}