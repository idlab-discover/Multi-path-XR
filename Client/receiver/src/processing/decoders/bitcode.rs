
use bitcode::{Decode, Encode};
use tracing::warn;

use shared_utils::types::Point3D;

use super::DecodeResult;


#[derive(Encode, Decode)]
pub struct BitcodeData {
    pub points: Vec<Point3D>,
}

pub fn decode_bc_one_from_bytes(data: Vec<u8>) -> DecodeResult {
    // Make sure we at least have the 3 header bytes plus some payload
    if data.len() < 3 {
        warn!("Not enough data to contain BC1 header");
        return Ok((1, Vec::new(), Vec::new()));
    }
    
    // Skip the identifier bytes
    let bitcode_data_bytes = &data[3..];


    let bitcode_data = match bitcode::decode::<BitcodeData>(bitcode_data_bytes) {
        Ok(decoded) => decoded,
        Err(err) => {
            warn!("Failed to decode payload: {}", err);
            return Ok((1, Vec::new(), Vec::new()));
        },
    };

    if bitcode_data.points.is_empty() {
        // If there are no vertices, return no error and empty points.
        return Ok((0, Vec::new(), Vec::new()))
    }

    // Extract the vertex and color arrays and flatten them
    let mut coords = Vec::with_capacity(bitcode_data.points.len()); // f32's in [x1,y1,z1, x2,y2,z2, ...]
    let mut colors = Vec::with_capacity(bitcode_data.points.len());
    for point in bitcode_data.points {
        // Add the coordinates to the coords array
        coords.push(point.x);
        coords.push(point.y);
        coords.push(point.z);
        // Add the colors to the colors array
        colors.push(point.r);
        colors.push(point.g);
        colors.push(point.b);
    }

    //=== Return success with 0 errors
    Ok((0, coords, colors))
}
