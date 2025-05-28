use tracing::error;
pub use draco_wrapper::decode_draco;

use super::DecodeResult;

pub fn decode_draco_from_bytes(data: Vec<u8>) -> DecodeResult {
    // info!("Decoding Draco data of length: {}", data.len());
    // Call the decode function from the DracoWrapper
    match decode_draco(data) {
        Ok((vertices, colors)) => {
            // info!("Successfully decoded Draco data");
            // No errors, return 0 errors, along with decoded vertices and colors
            Ok((0, vertices, colors))
        }
        Err(e) => {
            error!("Error decoding Draco data: {}", e);
            // If there's an error, return 1 error and empty vectors
            Ok((1, Vec::new(), Vec::new()))
        }
    }
}
