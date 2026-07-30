use crate::types::StreamPayloadFormat;
use spatial_codecs::decoder::{decode_into, decode_to_points_vec};
use spatial_utils::splat::GaussianSplatF32;
use tracing::instrument;

use shared_utils::types::{SpatialFrameData, SpatialPayload};

#[instrument(skip_all)]
pub fn decode_data(
    raw_data: &[u8],
    input_format: StreamPayloadFormat,
) -> Result<SpatialFrameData, Box<dyn std::error::Error>> {
    let payload = match input_format {
        StreamPayloadFormat::DecodedPoints => {
            SpatialPayload::Points(decode_to_points_vec(raw_data)?)
        }
        StreamPayloadFormat::DecodedGaussianSplats => {
            let mut splats: Vec<GaussianSplatF32> = Vec::new();
            decode_into(raw_data, &mut splats)?;
            SpatialPayload::GaussianSplats(splats)
        }
        StreamPayloadFormat::Auto
        | StreamPayloadFormat::PreencodedPoints
        | StreamPayloadFormat::PreencodedGaussianSplats => {
            return Err(format!("{input_format:?} is not a decoded input format").into());
        }
    };

    Ok(SpatialFrameData {
        payload,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use spatial_codecs::encoder::{encode_from_points, EncodingFormat};
    use spatial_utils::color::Rgba8;

    #[test]
    fn decodes_gsplat_payload_to_spatial_frame() {
        let splat = GaussianSplatF32::new(
            [1.0, 2.0, 3.0],
            Rgba8::new(10, 20, 30, 40),
            [0.1, 0.2, 0.3],
            [1.0, 0.0, 0.0, 0.0],
        );
        let encoded = encode_from_points(vec![splat], EncodingFormat::Gsplat16).unwrap();

        let decoded = decode_data(&encoded, StreamPayloadFormat::DecodedGaussianSplats).unwrap();

        match decoded.payload {
            SpatialPayload::GaussianSplats(splats) => assert_eq!(splats.len(), 1),
            SpatialPayload::Points(_) => panic!("decoded splat payload as points"),
        }
    }
}
