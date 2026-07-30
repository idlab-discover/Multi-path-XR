use pcf::{
    frame::PcfHeader,
    types::{Flags, RenderPrimitive as PcfRenderPrimitive},
};
use shared_utils::types::{
    FrameData, FramePayloadContainer, FramePayloadMetadata, FrameRenderPrimitive,
};
use spatial_codecs::decoder::{decode_into, decode_to_flattened_vecs};
use spatial_utils::splat::GaussianSplatF32;
use tracing::error;

pub fn decode_data(
    send_time: u64,
    presentation_time: u64,
    payload_metadata: FramePayloadMetadata,
    data: &[u8],
) -> Result<FrameData, Box<dyn std::error::Error>> {
    let (payload, payload_metadata) = decode_payload_container(payload_metadata, data)?;
    let render_primitive = infer_render_primitive(payload_metadata.primitive, payload);

    match render_primitive {
        FrameRenderPrimitive::Points => decode_points(send_time, presentation_time, payload),
        FrameRenderPrimitive::GaussianSplats => {
            decode_gaussian_splats(send_time, presentation_time, payload)
        }
    }
}

fn decode_payload_container(
    payload_metadata: FramePayloadMetadata,
    data: &[u8],
) -> Result<(&[u8], FramePayloadMetadata), Box<dyn std::error::Error>> {
    match payload_metadata.container {
        FramePayloadContainer::Raw => Ok((data, payload_metadata)),
        FramePayloadContainer::Pcf => {
            let header = PcfHeader::parse(data).map_err(|e| format!("{}", e))?;
            if header.flags.contains(Flags::DELTA) {
                return Err("stateful PCF delta decode is not wired into the receiver yet".into());
            }
            let mut metadata = payload_metadata;
            metadata.primitive = header
                .render_primitive
                .map(|primitive| match primitive {
                    PcfRenderPrimitive::Points => FrameRenderPrimitive::Points,
                    PcfRenderPrimitive::GaussianSplats => FrameRenderPrimitive::GaussianSplats,
                })
                .unwrap_or_else(|| infer_render_primitive(metadata.primitive, header.payload));
            Ok((header.payload, metadata))
        }
    }
}

fn infer_render_primitive(declared: FrameRenderPrimitive, payload: &[u8]) -> FrameRenderPrimitive {
    if declared == FrameRenderPrimitive::GaussianSplats
        || payload.get(0..3) == Some(b"GSP")
            && payload
                .get(4)
                .is_some_and(|flags| (flags & 0b0000_0001) != 0)
    {
        FrameRenderPrimitive::GaussianSplats
    } else {
        FrameRenderPrimitive::Points
    }
}

fn decode_points(
    send_time: u64,
    presentation_time: u64,
    payload: &[u8],
) -> Result<FrameData, Box<dyn std::error::Error>> {
    let (error_count, vertices, colors) = match decode_to_flattened_vecs(payload) {
        Ok((vertices, colors)) => (0, vertices, colors),
        Err(e) => {
            error!("Decoding error: {}", e);
            (1, Vec::new(), Vec::new())
        }
    };

    let point_count = (vertices.len() / 3) as u64;

    Ok(FrameData {
        send_time,
        presentation_time,
        receive_time: 0,
        quality_index: None,
        render_primitive: FrameRenderPrimitive::Points,
        error_count,
        point_count,
        coordinates: vertices,
        colors,
        gaussian_scales: Vec::new(),
        gaussian_rotations: Vec::new(),
    })
}

fn decode_gaussian_splats(
    send_time: u64,
    presentation_time: u64,
    payload: &[u8],
) -> Result<FrameData, Box<dyn std::error::Error>> {
    let mut splats = Vec::<GaussianSplatF32>::new();
    let error_count = match decode_into(payload, &mut splats) {
        Ok(()) => 0,
        Err(e) => {
            error!("Gaussian splat decoding error: {}", e);
            splats.clear();
            1
        }
    };

    let mut coordinates = Vec::with_capacity(splats.len() * 3);
    let mut colors = Vec::with_capacity(splats.len() * 4);
    let mut gaussian_scales = Vec::with_capacity(splats.len() * 3);
    let mut gaussian_rotations = Vec::with_capacity(splats.len() * 4);

    for splat in &splats {
        coordinates.extend_from_slice(&splat.mean);
        colors.extend_from_slice(&[splat.rgba.r, splat.rgba.g, splat.rgba.b, splat.rgba.a]);
        gaussian_scales.extend_from_slice(&splat.scale);
        gaussian_rotations.extend_from_slice(&splat.rotation);
    }

    Ok(FrameData {
        send_time,
        presentation_time,
        receive_time: 0,
        quality_index: None,
        render_primitive: FrameRenderPrimitive::GaussianSplats,
        error_count,
        point_count: splats.len() as u64,
        coordinates,
        colors,
        gaussian_scales,
        gaussian_rotations,
    })
}
