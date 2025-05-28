
use tracing::error;
use tmf::TMFMesh;

use super::DecodeResult;

pub fn decode_tmf_from_bytes(data: Vec<u8>) -> DecodeResult {
    //=== Step 1: Try reading one mesh
    let (mesh, _name) = match TMFMesh::read_tmf_one(&mut &data[..]) {
        Ok(m) => m,
        Err(e) => {
            error!("Error decoding TMF data: {}", e);
            // If there's an error, we return 1 error and empty points.
            return Ok((1, Vec::new(), Vec::new()))
        }
    };

    //=== Step 2: Extract the vertex array and flatten into f32 coords
    let mut coords = Vec::new(); // f32's in [x1,y1,z1, x2,y2,z2, ...]
    let vertex_count = if let Some(verts) = mesh.get_vertices() {
        coords.reserve(verts.len() * 3);
        for v in verts.iter() {
            coords.push(v.0);
            coords.push(v.1);
            coords.push(v.2);
        }
        verts.len()
    } else {
        0
    };

    if vertex_count == 0 {
        // If there are no vertices, return no error and empty points.
        return Ok((0, Vec::new(), Vec::new()))
    }

    //=== Step 3: Extract color data and flatten into u8s in [r1,g1,b1, r2,g2,b2, ...]
    let mut colors = Vec::with_capacity(vertex_count * 3); // u8 in [r1,g1,b1, r2,g2,b2, ...]

    // If the "colors" custom data is present, decode it
    if let Some(cdata) = mesh.lookup_custom_data("colors") {
        // We stored them as CustomIntiger, so do as_intiger()
        if let Some((color_ints, _max_val)) = cdata.as_intiger() {
            // color_ints is the slice of u32, one per vertex
            let available_count = color_ints.len();

            // We'll fill exactly `vertex_count` points of color
            // If `available_count < vertex_count`, we fill the rest with zeros
            // If `available_count > vertex_count`, we ignore the extras

            let common_count = std::cmp::min(vertex_count, available_count);
            for &packed in color_ints.iter().take(common_count) {
                let r = ((packed >> 16) & 0xFF) as u8;
                let g = ((packed >>  8) & 0xFF) as u8;
                let b = ( packed        & 0xFF) as u8;
                colors.push(r);
                colors.push(g);
                colors.push(b);
            }
            // If color_ints are fewer than the vertices, fill with zeros
            if available_count < vertex_count {
                let missing = vertex_count - available_count;
                // For each missing point, push [0,0,0]
                for _ in 0..missing {
                    colors.push(0);
                    colors.push(0);
                    colors.push(0);
                }
            }
        } else {
            // The custom data wasn't an integer array. We'll just fill with zeros:
            for _ in 0..vertex_count {
                colors.push(0);
                colors.push(0);
                colors.push(0);
            }
        }
    } else {
        // No color data present at all => fill with zeros
        for _ in 0..vertex_count {
            colors.push(0);
            colors.push(0);
            colors.push(0);
        }
    }

    //=== Return success with 0 errors
    Ok((0, coords, colors))
}
