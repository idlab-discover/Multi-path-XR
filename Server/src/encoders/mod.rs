use spatial_codecs::encoder;

use shared_utils::types::{SpatialFrameData, SpatialPayload};

pub fn encode_data(
    spatial_frame: SpatialFrameData,
    encoding: encoder::EncodingFormat,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    match spatial_frame.payload {
        SpatialPayload::Points(points) => encoder::encode_from_points(points, encoding),
        SpatialPayload::GaussianSplats(splats) => {
            encoder::encode_from_points(splats, splat_encoding_format(encoding))
        }
    }
}

fn splat_encoding_format(encoding: encoder::EncodingFormat) -> encoder::EncodingFormat {
    match encoding {
        encoder::EncodingFormat::Ply
        | encoder::EncodingFormat::Gsplat16
        | encoder::EncodingFormat::Bitcode
        | encoder::EncodingFormat::Gzip
        | encoder::EncodingFormat::Zstd
        | encoder::EncodingFormat::Lz4
        | encoder::EncodingFormat::Snappy
        | encoder::EncodingFormat::Openzl => encoding,
        encoder::EncodingFormat::Draco
        | encoder::EncodingFormat::LASzip
        | encoder::EncodingFormat::Tmf
        | encoder::EncodingFormat::Sogp
        | encoder::EncodingFormat::Quantize => encoder::EncodingFormat::Gsplat16,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spatial_utils::{color::Rgba8, splat::GaussianSplatF32};

    #[test]
    fn encodes_splats_as_gsplat_when_requested_codec_is_point_only() {
        let frame = SpatialFrameData {
            payload: SpatialPayload::GaussianSplats(vec![GaussianSplatF32::new(
                [1.0, 2.0, 3.0],
                Rgba8::new(10, 20, 30, 40),
                [0.1, 0.2, 0.3],
                [1.0, 0.0, 0.0, 0.0],
            )]),
            ..Default::default()
        };

        let encoded = encode_data(frame, encoder::EncodingFormat::Draco).unwrap();

        assert!(encoded.starts_with(b"GSP"));
    }
}
