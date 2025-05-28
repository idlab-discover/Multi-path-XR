pub mod draco;
pub mod ply;
pub mod tmf;
pub mod bitcode;
use serde::{Deserialize, Serialize};
use tracing::instrument;

use shared_utils::types::PointCloudData;


// Provide an enum to represent the different encoding formats
#[derive(Debug, Serialize, Deserialize, PartialEq, Copy, Clone)]
pub enum EncodingFormat {
    Ply = 0,
    Draco,
    LASzip,
    Tmf,
    Bitcode
}

#[instrument(skip_all)]
pub fn encode_data(
    point_cloud: PointCloudData,
    encoding: EncodingFormat,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {


    match encoding {
        EncodingFormat::Ply => ply::encode_ply(point_cloud),
        EncodingFormat::Draco => draco::encode_draco(point_cloud),
        EncodingFormat::Tmf => tmf::encode_tmf(point_cloud),
        EncodingFormat::Bitcode => bitcode::encode_bitcode(point_cloud),
        _ => Err("Unsupported encoding format".into()),
    }
}
