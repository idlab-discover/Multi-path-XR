use bitcode::{Decode, Encode, encode as bt_encode};
use tracing::{debug, instrument};

use shared_utils::types::{Point3D, PointCloudData};

#[derive(Encode, Decode)]
pub struct BitcodeData {
    pub points: Vec<Point3D>,
}

#[instrument(skip_all)]
pub fn encode_bitcode(point_cloud: PointCloudData) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
   
    let bitcode_data = BitcodeData {
        points: point_cloud.points,
    };

    debug!("Bitcode data will have {} vertices", bitcode_data.points.len());

    let bitcode_raw = bt_encode(&bitcode_data);

    debug!("Encoded frame to {} bytes", bitcode_raw.len());

    // We know the final size is 3 + bitcode_raw.len()
    // So we can reserve that up front:
    let mut encoded = Vec::with_capacity(3 + bitcode_raw.len());
    encoded.extend_from_slice(b"BC1");
    encoded.extend_from_slice(&bitcode_raw);

    Ok(encoded)
}

