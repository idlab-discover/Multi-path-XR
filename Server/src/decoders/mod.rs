use tracing::instrument;

use shared_utils::types::PointCloudData;

pub mod ply;
pub mod draco;

#[instrument(skip_all)]
pub fn decode_data(raw_data: Vec<u8>) -> Result<PointCloudData, Box<dyn std::error::Error>> {
    if raw_data.is_empty() || raw_data.len() < 3 {
        return Err("Not enough data to contain header".into());
    }


    match &raw_data[0..3] {
        b"ply" => ply::decode_ply(raw_data),
        b"DRA" => draco::decode_draco(raw_data),
        //b"TMF" => tmf::decode_tmf_from_bytes(data)?,
        //b"BC1" => bitcode::decode_bc_one_from_bytes(data)?,
        _ => return Err("Unsupported data format".into()),
    }
}