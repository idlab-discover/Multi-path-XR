use crate::decoders;
use crate::encoders::encode_data;
use crate::services::stream_manager::StreamManager;
use crate::types::{StreamPayloadFormat, StreamSettings};
use bytes::Bytes;
use metrics::get_metrics;
use pcf::{frame::PcfHeader, types::RenderPrimitive as PcfRenderPrimitive};
use pre_encode::prep_for_encoding;
use prometheus::IntGauge;
use rayon::ThreadPool;
use shared_utils::types::{
    FramePayloadMetadata, FrameRenderPrimitive, FrameTaskData, SpatialFrameData, SpatialPayload,
};
use spatial_codecs::encoder::EncodingFormat;
use spatial_utils::sampling::chunker::percentage_chunks::partition_by_percentages;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tracing::{error, instrument};

pub mod aggregator;
pub mod filtering;
pub mod pre_encode;

const DEFAULT_PRESENTATION_TIME_OFFSET_US: u64 = 100_000;
const PLY_HEADER_SCAN_LIMIT: usize = 64 * 1024;

fn detect_stream_payload_format(data: &[u8]) -> StreamPayloadFormat {
    if data.starts_with(pcf::types::PCF_MAGIC) {
        return detect_pcf_payload_format(data);
    }

    if is_gsplat16_payload(data) {
        return StreamPayloadFormat::PreencodedGaussianSplats;
    }

    if is_gaussian_splat_ply(data) {
        return StreamPayloadFormat::DecodedGaussianSplats;
    }

    StreamPayloadFormat::DecodedPoints
}

fn detect_pcf_payload_format(data: &[u8]) -> StreamPayloadFormat {
    let Ok(header) = PcfHeader::parse(data) else {
        return StreamPayloadFormat::PreencodedPoints;
    };

    if let Some(render_primitive) = header.render_primitive {
        return match render_primitive {
            PcfRenderPrimitive::Points => StreamPayloadFormat::PreencodedPoints,
            PcfRenderPrimitive::GaussianSplats => StreamPayloadFormat::PreencodedGaussianSplats,
        };
    }

    if is_gsplat16_payload(header.payload) {
        StreamPayloadFormat::PreencodedGaussianSplats
    } else {
        StreamPayloadFormat::PreencodedPoints
    }
}

fn is_gsplat16_payload(data: &[u8]) -> bool {
    data.get(0..3) == Some(b"GSP") && data.get(4).is_some_and(|flags| (flags & 0b0000_0001) != 0)
}

fn is_gaussian_splat_ply(data: &[u8]) -> bool {
    let Some(vertex_properties) = ply_vertex_properties(data) else {
        return false;
    };

    has_all_properties(&vertex_properties, &["x", "y", "z"])
        && has_all_properties(&vertex_properties, &["scale_0", "scale_1", "scale_2"])
        && has_all_properties(&vertex_properties, &["rot_0", "rot_1", "rot_2", "rot_3"])
        && has_all_properties(&vertex_properties, &["opacity"])
        && (has_all_properties(&vertex_properties, &["f_dc_0", "f_dc_1", "f_dc_2"])
            || has_all_properties(&vertex_properties, &["red", "green", "blue"]))
}

fn has_all_properties(properties: &[String], required: &[&str]) -> bool {
    required
        .iter()
        .all(|name| properties.iter().any(|property| property == name))
}

fn ply_vertex_properties(data: &[u8]) -> Option<Vec<String>> {
    if data.get(0..3) != Some(b"ply") {
        return None;
    }

    let scan_len = data.len().min(PLY_HEADER_SCAN_LIMIT);
    let header_end = data[..scan_len]
        .windows(b"end_header".len())
        .position(|window| window == b"end_header")?
        + b"end_header".len();
    let header = std::str::from_utf8(&data[..header_end]).ok()?;

    let mut in_vertex_element = false;
    let mut vertex_properties = Vec::new();

    for line in header.lines().map(str::trim) {
        let mut parts = line.split_whitespace();
        match parts.next() {
            Some("element") => {
                in_vertex_element = parts.next() == Some("vertex");
            }
            Some("property") if in_vertex_element => {
                if let Some(name) = line.split_whitespace().last() {
                    vertex_properties.push(name.to_owned());
                }
            }
            Some("end_header") => break,
            _ => {}
        }
    }

    Some(vertex_properties)
}

fn resolve_stream_payload_format(
    stream_manager: &Arc<StreamManager>,
    settings: &StreamSettings,
    raw_data: &[u8],
) -> StreamPayloadFormat {
    if settings.input_format != StreamPayloadFormat::Auto {
        return settings.input_format;
    }

    let detected = detect_stream_payload_format(raw_data);
    let mut detected_settings = settings.clone();
    detected_settings.input_format = detected;
    stream_manager.update_stream_settings(detected_settings);
    detected
}

#[derive(Clone, Debug)]
pub struct ProcessingPipeline {
    pub thread_pool: Arc<ThreadPool>,
    pub decoding_time: IntGauge,
    pub process_to_buffer_time: IntGauge,
    pub frames_to_decode: IntGauge,
}

impl ProcessingPipeline {
    #[instrument(skip_all)]
    pub fn new(thread_pool: Arc<ThreadPool>) -> Self {
        let metrics = get_metrics();
        Self {
            thread_pool,
            decoding_time: metrics
                .get_or_create_gauge("decoding_time", "Time taken to decode a frame")
                .unwrap(),
            process_to_buffer_time: metrics
                .get_or_create_gauge(
                    "process_to_buffer_time",
                    "Time taken to process a frame and push it to the egress buffer where it will be combined with the other streams.",
                )
                .unwrap(),
            frames_to_decode: metrics
                .get_or_create_gauge("frames_to_decode", "Number of frames to be decoded")
                .unwrap(),
        }
    }

    #[instrument(skip_all)]
    pub fn decode(
        &self,
        raw_data: &[u8],
        input_format: StreamPayloadFormat,
    ) -> Result<SpatialFrameData, Box<dyn std::error::Error>> {
        decoders::decode_data(raw_data, input_format)
    }

    #[instrument(skip_all)]
    pub fn encode(
        &self,
        spatial_frame: SpatialFrameData,
        encoding: EncodingFormat,
    ) -> Result<FrameTaskData, Box<dyn std::error::Error>> {
        let creation_time = spatial_frame.creation_time;
        let presentation_time = spatial_frame.presentation_time;
        let primitive = spatial_frame.render_primitive();
        let data = encode_data(spatial_frame, encoding);

        match data {
            Ok(data) => Ok(FrameTaskData {
                send_time: creation_time,
                presentation_time,
                payload_metadata: FramePayloadMetadata {
                    primitive,
                    ..Default::default()
                },
                data,
                client_id: None,
                quality_index: None,
            }),
            Err(e) => Err(e),
        }
    }

    #[instrument(skip_all)]
    pub fn push_to_decoder(
        &self,
        raw_data: Vec<u8>,
        stream_manager: Arc<StreamManager>,
        stream_id: String,
    ) {
        self.push_bytes_to_decoder(Bytes::from(raw_data), stream_manager, stream_id);
    }

    #[instrument(skip_all)]
    pub fn push_bytes_to_decoder(
        &self,
        raw_data: Bytes,
        stream_manager: Arc<StreamManager>,
        stream_id: String,
    ) {
        let settings = stream_manager.get_stream_settings(&stream_id);
        if !settings.process_incoming_frames {
            return;
        }

        let input_format = resolve_stream_payload_format(&stream_manager, &settings, &raw_data);
        if settings.decode_bypass {
            self.spawn_preencoded_frame(raw_data, stream_manager, stream_id);
            return;
        }

        if input_format.is_preencoded() {
            self.spawn_preencoded_frame(raw_data, stream_manager, stream_id);
            return;
        }

        match input_format {
            StreamPayloadFormat::Auto => unreachable!("auto input format must be resolved"),
            StreamPayloadFormat::PreencodedPoints
            | StreamPayloadFormat::PreencodedGaussianSplats => {
                unreachable!("preencoded input formats return before decoded dispatch")
            }
            StreamPayloadFormat::DecodedPoints => {
                self.spawn_decoded_frame(
                    raw_data,
                    stream_manager,
                    stream_id,
                    settings.presentation_time_offset,
                    input_format,
                );
            }
            StreamPayloadFormat::DecodedGaussianSplats => {
                self.spawn_decoded_frame(
                    raw_data,
                    stream_manager,
                    stream_id,
                    settings.presentation_time_offset,
                    input_format,
                );
            }
        }
    }

    fn spawn_preencoded_frame(
        &self,
        raw_data: Bytes,
        stream_manager: Arc<StreamManager>,
        stream_id: String,
    ) {
        let processing_pipeline = Arc::new(self.clone());
        let thread_pool = Arc::clone(&self.thread_pool);
        thread_pool.spawn(move || {
            processing_pipeline.process_frame_raw(raw_data, stream_manager, stream_id);
        });
    }

    fn spawn_decoded_frame(
        &self,
        raw_data: Bytes,
        stream_manager: Arc<StreamManager>,
        stream_id: String,
        presentation_time_offset: Option<u64>,
        input_format: StreamPayloadFormat,
    ) {
        let processing_pipeline = Arc::new(self.clone());
        let decoding_time = self.decoding_time.clone();
        let process_to_buffer_time = self.process_to_buffer_time.clone();
        let frames_to_decode = self.frames_to_decode.clone();
        let thread_pool = Arc::clone(&self.thread_pool);

        thread_pool.spawn(move || {
            ProcessingPipeline::handle_decoding_and_processing(
                processing_pipeline,
                &raw_data,
                stream_manager,
                stream_id,
                presentation_time_offset,
                input_format,
                decoding_time,
                process_to_buffer_time,
                frames_to_decode,
            );
        });
    }

    #[allow(clippy::too_many_arguments)]
    #[instrument(skip_all)]
    fn handle_decoding_and_processing(
        processing_pipeline: Arc<ProcessingPipeline>,
        raw_data: &[u8],
        stream_manager: Arc<StreamManager>,
        stream_id: String,
        presentation_time_offset: Option<u64>,
        input_format: StreamPayloadFormat,
        decoding_time: IntGauge,
        process_to_buffer_time: IntGauge,
        frames_to_decode: IntGauge,
    ) {
        let start_time = Instant::now();
        let mut spatial_frame = match processing_pipeline.decode(raw_data, input_format) {
            Ok(frame) => frame,
            Err(e) => {
                error!("Decoding failed: {:?}", e);
                return;
            }
        };

        if let Some(pto) = presentation_time_offset {
            spatial_frame.presentation_time = spatial_frame.creation_time.saturating_add(pto);
        }

        decoding_time.set(start_time.elapsed().as_micros() as i64);

        let start_time = Instant::now();
        frames_to_decode.inc();
        processing_pipeline.process_frame(spatial_frame, stream_manager, stream_id);
        process_to_buffer_time.set(start_time.elapsed().as_micros() as i64);
    }

    #[instrument(skip_all, fields(stream_id = %stream_id))]
    pub fn process_frame(
        &self,
        spatial_frame: SpatialFrameData,
        stream_manager: Arc<StreamManager>,
        stream_id: String,
    ) {
        let settings = stream_manager.get_stream_settings(&stream_id);
        let thread_pool = Arc::clone(&self.thread_pool);

        for egress in stream_manager.get_egresses(&settings.egress_protocols) {
            if settings.aggregator_bypass {
                let frame_prepped = prep_for_encoding(
                    spatial_frame.clone(),
                    &settings,
                    Some(egress.max_number_of_primitives()),
                );
                if let Some(ref percentages) = settings.max_primitive_percentages {
                    let sub_frames =
                        split_spatial_frame_by_percentages(&frame_prepped, percentages)
                            .expect("invalid primitive percentage split");

                    for (index, sub_frame) in sub_frames.into_iter().enumerate() {
                        let quality_index = settings
                            .quality_index
                            .map(|index_value| index_value + index as u32);
                        let ring_buffer_bypass = settings.ring_buffer_bypass;
                        let client_id = settings.client_id;
                        let stream_id = match (client_id, quality_index) {
                            (Some(cid), Some(qid)) => format!("client_{}_{}", cid, qid),
                            _ => stream_id.clone(),
                        };

                        let egress_clone = egress.clone();
                        let thread_pool = thread_pool.clone();
                        let processing_pipeline_clone = self.clone();

                        thread_pool.spawn(move || {
                            let bytes = processing_pipeline_clone
                                .encode(sub_frame.clone(), egress_clone.encoding_format())
                                .unwrap()
                                .data;
                            egress_clone.push_encoded_frame(
                                bytes,
                                stream_id,
                                sub_frame.creation_time,
                                sub_frame.presentation_time,
                                ring_buffer_bypass,
                                None,
                                client_id,
                                quality_index,
                            );
                        });
                    }
                } else {
                    let bytes = self
                        .encode(frame_prepped.clone(), egress.encoding_format())
                        .unwrap()
                        .data;
                    egress.push_encoded_frame(
                        bytes,
                        stream_id.clone(),
                        frame_prepped.creation_time,
                        frame_prepped.presentation_time,
                        settings.ring_buffer_bypass,
                        None,
                        settings.client_id,
                        settings.quality_index,
                    );
                }
            } else {
                egress.push_spatial_frame(spatial_frame.clone(), stream_id.clone());
            }
        }
    }

    #[instrument(skip_all)]
    pub fn process_frame_raw(
        &self,
        raw_data: Bytes,
        stream_manager: Arc<StreamManager>,
        stream_id: String,
    ) {
        let settings = stream_manager.get_stream_settings(&stream_id);

        let since_the_epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards");
        let creation_time = since_the_epoch.as_micros() as u64;
        let presentation_time = creation_time.saturating_add(
            settings
                .presentation_time_offset
                .unwrap_or(DEFAULT_PRESENTATION_TIME_OFFSET_US),
        );
        let payload_metadata = preencoded_payload_metadata(settings.input_format);

        for egress in stream_manager.get_egresses(&settings.egress_protocols) {
            egress.push_encoded_frame(
                raw_data.to_vec(),
                stream_id.clone(),
                creation_time,
                presentation_time,
                settings.ring_buffer_bypass,
                payload_metadata,
                settings.client_id,
                settings.quality_index,
            );
        }
    }
}

fn preencoded_payload_metadata(input_format: StreamPayloadFormat) -> Option<FramePayloadMetadata> {
    match input_format {
        StreamPayloadFormat::DecodedGaussianSplats
        | StreamPayloadFormat::PreencodedGaussianSplats => Some(FramePayloadMetadata {
            primitive: FrameRenderPrimitive::GaussianSplats,
            ..Default::default()
        }),
        StreamPayloadFormat::DecodedPoints | StreamPayloadFormat::PreencodedPoints => {
            Some(FramePayloadMetadata {
                primitive: FrameRenderPrimitive::Points,
                ..Default::default()
            })
        }
        StreamPayloadFormat::Auto => None,
    }
}

fn split_spatial_frame_by_percentages(
    frame: &SpatialFrameData,
    percentages: &[u8],
) -> Result<Vec<SpatialFrameData>, &'static str> {
    let payloads: Vec<SpatialPayload> = match &frame.payload {
        SpatialPayload::Points(points) => partition_by_percentages(points, percentages)?
            .into_iter()
            .map(|bucket| SpatialPayload::Points(bucket.into_iter().cloned().collect()))
            .collect(),
        SpatialPayload::GaussianSplats(splats) => partition_by_percentages(splats, percentages)?
            .into_iter()
            .map(|bucket| SpatialPayload::GaussianSplats(bucket.into_iter().cloned().collect()))
            .collect(),
    };

    Ok(payloads
        .into_iter()
        .map(|payload| SpatialFrameData {
            payload,
            creation_time: frame.creation_time,
            presentation_time: frame.presentation_time,
            error_count: frame.error_count,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::{detect_stream_payload_format, preencoded_payload_metadata};
    use crate::types::StreamPayloadFormat;
    use pcf::{
        frame::{PcfFrameMeta, PcfHeader},
        types::RenderPrimitive,
    };
    use shared_utils::types::FrameRenderPrimitive;

    #[test]
    fn classifies_raw_preencoded_payloads() {
        assert_eq!(
            detect_stream_payload_format(b"GSP\x01\x01payload"),
            StreamPayloadFormat::PreencodedGaussianSplats
        );
        assert_eq!(
            detect_stream_payload_format(b"GSP\x01\x00payload"),
            StreamPayloadFormat::DecodedPoints
        );
    }

    #[test]
    fn classifies_pcf_by_render_primitive() {
        let mut frame = Vec::new();
        PcfHeader::write_frame_to(
            &mut frame,
            &PcfFrameMeta {
                render_primitive: Some(RenderPrimitive::GaussianSplats),
                ..Default::default()
            },
            b"payload",
        )
        .unwrap();

        assert_eq!(
            detect_stream_payload_format(&frame),
            StreamPayloadFormat::PreencodedGaussianSplats
        );
    }

    #[test]
    fn classifies_pcf_without_primitive_from_inner_payload() {
        let mut frame = Vec::new();
        PcfHeader::write_frame_to(&mut frame, &PcfFrameMeta::default(), b"GSP\x01\x01payload")
            .unwrap();

        assert_eq!(
            detect_stream_payload_format(&frame),
            StreamPayloadFormat::PreencodedGaussianSplats
        );
    }

    #[test]
    fn classifies_point_ply_as_decoded_points() {
        let point_ply = b"ply\nformat binary_little_endian 1.0\nelement vertex 1\nproperty float x\nproperty float y\nproperty float z\nproperty uchar red\nproperty uchar green\nproperty uchar blue\nend_header\n";

        assert_eq!(
            detect_stream_payload_format(point_ply),
            StreamPayloadFormat::DecodedPoints
        );
    }

    #[test]
    fn classifies_gaussian_splat_ply_from_vertex_properties() {
        let splat_ply = b"ply\nformat binary_little_endian 1.0\nelement vertex 1\nproperty float x\nproperty float y\nproperty float z\nproperty float f_dc_0\nproperty float f_dc_1\nproperty float f_dc_2\nproperty float opacity\nproperty float scale_0\nproperty float scale_1\nproperty float scale_2\nproperty float rot_0\nproperty float rot_1\nproperty float rot_2\nproperty float rot_3\nend_header\n";

        assert_eq!(
            detect_stream_payload_format(splat_ply),
            StreamPayloadFormat::DecodedGaussianSplats
        );
    }

    #[test]
    fn ignores_splat_like_properties_outside_vertex_element() {
        let face_ply = b"ply\nformat ascii 1.0\nelement vertex 1\nproperty float x\nproperty float y\nproperty float z\nelement face 1\nproperty float f_dc_0\nproperty float f_dc_1\nproperty float f_dc_2\nproperty float opacity\nproperty float scale_0\nproperty float scale_1\nproperty float scale_2\nproperty float rot_0\nproperty float rot_1\nproperty float rot_2\nproperty float rot_3\nend_header\n";

        assert_eq!(
            detect_stream_payload_format(face_ply),
            StreamPayloadFormat::DecodedPoints
        );
    }

    #[test]
    fn raw_bypass_metadata_preserves_decoded_gaussian_splat_format() {
        assert_eq!(
            preencoded_payload_metadata(StreamPayloadFormat::DecodedGaussianSplats)
                .unwrap()
                .primitive,
            FrameRenderPrimitive::GaussianSplats
        );
        assert_eq!(
            preencoded_payload_metadata(StreamPayloadFormat::DecodedPoints)
                .unwrap()
                .primitive,
            FrameRenderPrimitive::Points
        );
    }
}
