use std::sync::{Arc, RwLock};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};
use crate::types::FrameData;
use circular_buffer::CircularBuffer;
use metrics::get_metrics;
use prometheus::IntGauge;
use tracing::info;

pub struct Storage {
    buffers: RwLock<HashMap<String, Arc<RwLock<CircularBuffer<30, FrameData>>>>>,
    last_consumed_point_counts: RwLock<HashMap<String, u64>>,
    pub reception_time_flute: IntGauge,
    pub frames_consumed_total: IntGauge,
    pub frames_received_total: IntGauge,
    pub frames_skipped_total: IntGauge,
    pub current_backlog: IntGauge,
    pub send_to_receive_time_diff: IntGauge,
    pub send_to_consume_time_diff: IntGauge,
    pub receive_to_consume_time_diff: IntGauge,
    pub point_count_metric: IntGauge,
    pub decode_time: IntGauge,
    pub total_point_count: IntGauge,
    pub quality_metric: IntGauge,
}

impl Default for Storage {
    fn default() -> Self {
        Self::new()
    }
}

impl Storage {
    pub fn new() -> Self {
        let metrics = get_metrics();

        // Example metrics:
        let reception_time_flute = metrics
            .get_or_create_gauge(
                "reception_time_flute",
                "Time (ms) it took to receive a FLUTE object",
            )
            .expect("Failed to create reception_time_flute gauge");

        let frames_consumed_total = metrics
            .get_or_create_gauge(
                "frames_consumed_total",
                "Total number of frames consumed properly",
            )
            .expect("Failed to create frames_consumed_total gauge");


        let frames_received_total = metrics
        .get_or_create_gauge(
            "frames_received_total",
            "Total number of frames that have been received",
        )
        .expect("Failed to create frames_received_total gauge");

        let frames_skipped_total = metrics
            .get_or_create_gauge(
                "frames_skipped_total",
                "Total number of frames skipped due to backlog",
            )
            .expect("Failed to create frames_skipped_total gauge");

        let current_backlog = metrics
            .get_or_create_gauge(
                "current_backlog",
                "Current maximum backlog across all streams",
            )
            .expect("Failed to create current_backlog gauge");

            let send_to_receive_time_diff = metrics
            .get_or_create_gauge(
                "send_to_receive_time_diff",
                "Difference (ms) between send time and receive time of a frame",
            )
            .expect("Failed to create send_to_receive_time_diff gauge");

        let send_to_consume_time_diff = metrics
            .get_or_create_gauge(
                "send_to_consume_time_diff",
                "Difference (ms) between send time and consume time of a frame",
            )
            .expect("Failed to create send_to_consume_time_diff gauge");

        let receive_to_consume_time_diff = metrics
            .get_or_create_gauge(
                "receive_to_consume_time_diff",
                "Difference (ms) between receive time and consume time of a frame",
            )
            .expect("Failed to create receive_to_consume_time_diff gauge");

        let point_count_metric = metrics
            .get_or_create_gauge(
                "point_count_metric",
                "Number of points in the last consumed frame",
            )
            .expect("Failed to create point_count_metric gauge");

        let decode_time = metrics.get_or_create_gauge(
            "decoding_time", 
            "Time taken to decode a frame").unwrap();

        let total_point_count = metrics.get_or_create_gauge(
                "total_point_count",
                "Total concurrent point count across all streams",
            ).unwrap();

        let quality_metric = metrics
            .get_or_create_gauge(
                "quality_metric",
                "Quality id of the stream",
            )
            .expect("Failed to create quality_metric gauge");

        Storage {
            buffers: RwLock::new(HashMap::new()),
            last_consumed_point_counts: RwLock::new(HashMap::new()),
            reception_time_flute,
            frames_consumed_total,
            frames_received_total,
            frames_skipped_total,
            current_backlog,
            send_to_receive_time_diff,
            send_to_consume_time_diff,
            receive_to_consume_time_diff,
            point_count_metric,
            decode_time,
            total_point_count,
            quality_metric,
        }
    }

    pub fn insert_frame(&self, stream_id: String, mut frame: FrameData) {
        // info!("Inserting frame with presentation time: {}", frame.presentation_time);
        // Check if the presentation time is 0
        if frame.presentation_time == 0 {
            // Overwrite the presentation time with the current time
            let current_time = match SystemTime::now().duration_since(UNIX_EPOCH) {
                Ok(duration) => duration.as_micros() as u64,
                Err(_) => return,
            };
            frame.presentation_time = current_time;
        }
        let mut buffers = self.buffers.write().unwrap();
        let buffer = buffers.entry(stream_id.clone()).or_insert_with(|| {
            Arc::new(RwLock::new(CircularBuffer::new()))
        });
        {
            let mut b = buffer.write().unwrap();
            if b.is_full() {
                // The first frame will be dropped by this circular buffer
                self.frames_skipped_total.inc();
            }
            b.push_back(frame)
        }
        self.frames_received_total.inc();
    }

    pub fn get_stream_ids(&self) -> Vec<String> {
        let buffers = self.buffers.read().unwrap();
        buffers.keys().cloned().collect()
    }

    pub fn get_frame_count(&self, stream_id: &String) -> usize {
        let buffers = self.buffers.read().unwrap();
        if let Some(buffer) = buffers.get(stream_id) {
            buffer.read().unwrap().len()
        } else {
            0
        }
    }

    pub fn get_highest_frame_count(&self) -> usize {
        let buffers = self.buffers.read().unwrap();
        buffers.values().map(|buffer| buffer.read().unwrap().len()).max().unwrap_or(0)
    }

    /// Remove up to `count` oldest frames from the buffer for `stream_id`.
    /// Returns the number of frames actually removed.
    pub fn remove_oldest_frames(&self, stream_id: &str, count: usize) -> usize {
        // Clone Arc so we can lock it outside the read-guard
        let buffer = {
            let buffers = self.buffers.read().unwrap();
            buffers.get(stream_id).cloned()
        };

        if let Some(buffer) = buffer {
            let mut buffer = buffer.write().unwrap();
            let mut removed = 0;
            for _ in 0..count {
                if buffer.is_empty() {
                    break;
                }
                buffer.pop_front();
                removed += 1;
                self.frames_skipped_total.inc();
            }
            removed
        } else {
            0
        }
    }

    /// Consume the "best" frame (closest in time to 'now') from the given stream,
    /// optionally removing older frames if the buffer is too big.
    pub fn consume_frame(&self, stream_id: &String) -> Option<FrameData> {
        let buffer = {
            let buffers = self.buffers.read().unwrap();
            buffers.get(stream_id).cloned()
        };

        if let Some(buffer) = buffer {
            let mut buffer = buffer.write().unwrap();
            if buffer.is_empty() {
                return None;
            }

            // If the buffer is bigger than 2, remove frames older than 5 seconds
            // (we can tweak these numbers as needed)
            if buffer.len() > 2 {
                // Current time (in us)
                let current_time_us = match SystemTime::now().duration_since(UNIX_EPOCH) {
                    Ok(dur) => dur.as_micros() as u64,
                    Err(_) => return None,
                };
                let five_seconds_ago = current_time_us.saturating_sub(5_000_000);

                // Repeatedly pop the front if it’s older than `five_seconds_ago`
                while buffer.len() > 1 {
                    if let Some(front_frame) = buffer.front() {
                        if front_frame.presentation_time < five_seconds_ago {
                            // remove it
                            info!("Removing frame older than 5s for stream_id = {}", stream_id);
                            buffer.pop_front();
                            self.frames_skipped_total.inc();
                        } else {
                            // if not older than 5s, break out
                            break;
                        }
                    }
                }
                // If buffer is empty after cleaning up older frames, then return
                if buffer.is_empty() {
                    return None;
                }
            }

            // We want the frame with presentation_time *closest* to now.
            // We'll do the same logic as before.
            let current_time = match SystemTime::now().duration_since(UNIX_EPOCH) {
                Ok(duration) => duration.as_micros() as u64,
                Err(_) => return None,
            };

            if buffer.len() > 1 {
                let mut smallest_diff: u64 = u64::MAX;
                let mut frame_index: usize = 0;

                for (current_index, frame) in buffer.iter().enumerate() {
                    let diff = (frame.presentation_time as i64 - current_time as i64).unsigned_abs();
                    if diff < smallest_diff {
                        smallest_diff = diff;
                        frame_index = current_index;
                    }
                }

                // Pop front until the closest frame is at the front
                // This is a simple catch-up strategy
                if frame_index > 0 {
                    for _ in 0..frame_index {
                        buffer.pop_front();
                        self.frames_skipped_total.inc();
                    }
                    info!("Skipped {} frames for stream_id = {} (catch-up).", frame_index, stream_id);
                }
            }


            self.frames_consumed_total.inc();

            // Pop and store the "best" frame
            let consumed_frame = buffer.pop_front();

            // Calculate and update our new metrics using the consumed frame
            if let Some(ref frame) = consumed_frame {
                let send_to_consume = current_time.saturating_sub(frame.send_time);
                let receive_to_consume = current_time.saturating_sub(frame.receive_time);

                self.send_to_consume_time_diff.set(send_to_consume as i64);
                self.receive_to_consume_time_diff.set(receive_to_consume as i64);
                self.point_count_metric.set(frame.point_count as i64);

                self.last_consumed_point_counts
                    .write()
                    .unwrap()
                    .insert(stream_id.clone(), frame.point_count);

                // Calculate the total point count across all streams
                let total_point_count = self.get_total_point_count();
                self.total_point_count.set(total_point_count as i64);

            }

            // Finally, return the consumed frame
            consumed_frame
        } else {
            None
        }
    }

    /// Calculates the total concurrent point count across all streams,
    /// using the last frame of each buffer.
    pub fn get_total_point_count(&self) -> u64 {
        let map = self.last_consumed_point_counts.read().unwrap();
        map.values().sum()
    }
}
