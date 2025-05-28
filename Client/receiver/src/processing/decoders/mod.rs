pub mod ply;
pub mod draco;
pub mod tmf;
pub mod bitcode;

use tracing::error;

use crate::types::FrameData;

type DecodeResult = Result<(u64, Vec<f32>, Vec<u8>), Box<dyn std::error::Error>>;

pub fn decode_data(send_time: u64, presentation_time: u64, data: Vec<u8>) -> Result<FrameData, Box<dyn std::error::Error>> {
    let (error_count, vertices, colors) = if data.is_empty() || data.len() < 3 {
        error!("Data is empty or too short, returning error");
        // If the data is empty or too short, return an error
        (1, Vec::new(), Vec::new())
    } else {
        match &data[0..3] {
            b"ply" => ply::decode_ply_from_bytes(data)?,
            b"DRA" => draco::decode_draco_from_bytes(data)?,
            b"TMF" => tmf::decode_tmf_from_bytes(data)?,
            b"BC1" => bitcode::decode_bc_one_from_bytes(data)?,
            _ => return Err("Unsupported data format".into()),
        }
    };
    let point_count = (vertices.len() / 3) as u64;

    Ok(FrameData {
        send_time,
        presentation_time,
        receive_time: 0,
        error_count,
        point_count,
        coordinates: vertices,
        colors,
    })
}
