use crate::processing::pre_encode::prep_for_encoding;
use crate::services::stream_manager::StreamManager;
use circular_buffer::CircularBuffer;
use metrics::get_metrics;
use prometheus::IntGauge;
use shared_utils::types::{FrameRenderPrimitive, SpatialFrameData, SpatialPayload};
use spatial_utils::point::Point3D;
use spatial_utils::sampling::exact_random::exact_random_sampling;
use spatial_utils::splat::GaussianSplatF32;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{debug, instrument};

#[derive(Debug)]
pub struct SpatialFrameAggregator {
    latest_frames: Mutex<HashMap<String, CircularBuffer<10, SpatialFrameData>>>,
    stream_manager: Arc<StreamManager>,
    has_update: Mutex<bool>,
    max_age: Mutex<u64>,
    dropped_after_insertion: IntGauge,
    dropped_because_late_insertion: IntGauge,
    dropped_old_age: IntGauge,
}

impl SpatialFrameAggregator {
    #[instrument(skip_all)]
    pub fn new(stream_manager: Arc<StreamManager>) -> Self {
        let metrics = get_metrics();

        Self {
            latest_frames: Mutex::new(HashMap::new()),
            stream_manager,
            has_update: Mutex::new(false),
            max_age: Mutex::new(5_000_000),
            dropped_after_insertion: metrics.get_or_create_gauge("dropped_after_insertion", "The number of frames that were dropped before a newer frame was inserted").unwrap(),
            dropped_because_late_insertion: metrics.get_or_create_gauge("dropped_because_late_insertion", "The number of frames that were dropped because they were older than the latest transmitted frame").unwrap(),
            dropped_old_age: metrics.get_or_create_gauge("dropped_old_age", "The number of frames that were dropped because they were too old").unwrap(),
        }
    }

    #[instrument(skip_all, fields(stream_id = %stream_id))]
    pub fn update_spatial_frame(&self, stream_id: String, spatial_frame: SpatialFrameData) {
        let mut guard = self.latest_frames.lock().unwrap();
        debug!("Updating spatial frame for stream {}", stream_id);

        if spatial_frame.is_empty() {
            debug!("Empty spatial frame received, removing entry");
            guard.remove(&stream_id);
            return;
        }

        let buffer = guard.entry(stream_id).or_default();
        if buffer.is_empty() {
            buffer.push_back(spatial_frame);
            *self.has_update.lock().unwrap() = true;
            return;
        }

        let newest_time = buffer.back().unwrap().presentation_time;
        let oldest_time = buffer.front().unwrap().presentation_time;
        let new_time = spatial_frame.presentation_time;

        if new_time >= newest_time {
            debug!("New frame is >= newest_time => push_back");
            if buffer.is_full() {
                self.dropped_after_insertion.inc();
                buffer.pop_front();
            }
            buffer.push_back(spatial_frame);
            *self.has_update.lock().unwrap() = true;
            return;
        }

        if new_time <= oldest_time {
            debug!("New frame is <= oldest_time => push_front");
            if buffer.is_full() {
                self.dropped_because_late_insertion.inc();
                return;
            }
            buffer.push_front(spatial_frame);
            *self.has_update.lock().unwrap() = true;
            return;
        }

        let mut temp = Vec::with_capacity(buffer.len() + 1);
        while let Some(existing) = buffer.pop_front() {
            temp.push(existing);
        }

        let insert_pos = temp
            .iter()
            .position(|frame| new_time < frame.presentation_time)
            .unwrap_or(temp.len());
        temp.insert(insert_pos, spatial_frame);

        if temp.len() > buffer.capacity() {
            self.dropped_after_insertion.inc();
            temp.remove(0);
        }

        for frame in temp {
            buffer.push_back(frame);
        }

        *self.has_update.lock().unwrap() = true;
    }

    #[instrument(skip_all)]
    pub fn generate_combined_spatial_frame(
        &self,
        max_number_of_primitives: u64,
    ) -> SpatialFrameData {
        let mut error_count = 0;

        let since_the_epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards");
        let current_time = since_the_epoch.as_micros() as u64;

        let mut guard = self.latest_frames.lock().unwrap();

        if guard.is_empty() {
            return empty_spatial_frame(current_time, 1);
        }

        if !*self.has_update.lock().unwrap() {
            return empty_spatial_frame(current_time, 1);
        }

        debug!("Aggregating spatial frames");

        let max_age = *self.max_age.lock().unwrap();
        let mut max_presentation_time = 0;
        let mut latest_creation_time = 0;
        let mut streams_to_remove = Vec::new();
        let mut combined_points: Vec<Point3D> = Vec::new();
        let mut combined_splats: Vec<GaussianSplatF32> = Vec::new();
        let mut target_primitive: Option<FrameRenderPrimitive> = None;
        let mut at_least_one_has_more_buffered = false;

        for (stream_id, buffer) in guard.iter_mut() {
            if buffer.is_empty() {
                debug!(
                    "Empty buffer received, removing entry for stream: {}",
                    stream_id
                );
                streams_to_remove.push(stream_id.clone());
                continue;
            }

            let spatial_frame = buffer.front().unwrap();
            if spatial_frame.is_empty() {
                debug!(
                    "Empty spatial frame received, removing entry for stream: {}",
                    stream_id
                );
                streams_to_remove.push(stream_id.clone());
                continue;
            }

            let overtime = current_time.saturating_sub(spatial_frame.presentation_time);
            if overtime > max_age {
                debug!(
                    "Spatial frame is too old, removing entry for stream: {}",
                    stream_id
                );
                buffer.pop_front();
                if buffer.is_empty() {
                    streams_to_remove.push(stream_id.clone());
                }
                self.dropped_old_age.inc();
                continue;
            }

            let spatial_frame = buffer.pop_front().unwrap();
            if !buffer.is_empty() {
                at_least_one_has_more_buffered = true;
            }

            let primitive = spatial_frame.render_primitive();
            if let Some(target) = target_primitive {
                if target != primitive {
                    error_count += spatial_frame.error_count + 1;
                    continue;
                }
            } else {
                target_primitive = Some(primitive);
            }

            if spatial_frame.presentation_time > max_presentation_time {
                max_presentation_time = spatial_frame.presentation_time;
            }

            if spatial_frame.creation_time > latest_creation_time {
                latest_creation_time = spatial_frame.creation_time;
            }

            error_count += spatial_frame.error_count;
            let settings = self.stream_manager.get_stream_settings(stream_id);
            let spatial_frame = prep_for_encoding(spatial_frame, &settings, None);

            match spatial_frame.payload {
                SpatialPayload::Points(points) => combined_points.extend(points),
                SpatialPayload::GaussianSplats(splats) => combined_splats.extend(splats),
            }
        }

        for stream_id in streams_to_remove {
            guard.remove(&stream_id);
        }

        if !at_least_one_has_more_buffered {
            *self.has_update.lock().unwrap() = false;
        }

        drop(guard);

        let payload = match target_primitive {
            Some(FrameRenderPrimitive::Points) if !combined_points.is_empty() => {
                if combined_points.len() > max_number_of_primitives as usize {
                    combined_points =
                        exact_random_sampling(&combined_points, max_number_of_primitives as usize);
                }
                SpatialPayload::Points(combined_points)
            }
            Some(FrameRenderPrimitive::GaussianSplats) if !combined_splats.is_empty() => {
                if combined_splats.len() > max_number_of_primitives as usize {
                    combined_splats =
                        exact_random_sampling(&combined_splats, max_number_of_primitives as usize);
                }
                SpatialPayload::GaussianSplats(combined_splats)
            }
            _ => return empty_spatial_frame(current_time, error_count + 1),
        };

        SpatialFrameData {
            payload,
            creation_time: if latest_creation_time > 0 {
                latest_creation_time
            } else {
                current_time
            },
            presentation_time: if max_presentation_time > 0 {
                max_presentation_time
            } else {
                current_time
            },
            error_count,
        }
    }
}

fn empty_spatial_frame(current_time: u64, error_count: u64) -> SpatialFrameData {
    SpatialFrameData {
        payload: SpatialPayload::Points(Vec::new()),
        creation_time: current_time,
        presentation_time: current_time,
        error_count,
    }
}
