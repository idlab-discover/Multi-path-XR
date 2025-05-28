use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use shared_utils::types::{Point3D, PointCloudData};
use crate::services::stream_manager::StreamManager;
use super::sampling::exact_random_sampling;
use metrics::get_metrics;
use nalgebra::{Vector3, Rotation3};
use prometheus::IntGauge;
use tracing::{instrument, debug};
use circular_buffer::CircularBuffer;

#[derive(Debug)]
pub struct PointCloudAggregator {
    latest_point_clouds: Mutex<HashMap<String, CircularBuffer<10,PointCloudData>>>,
    stream_manager: Arc<StreamManager>,
    has_update: Mutex<bool>,
    max_age: Mutex<u64>,
    dropped_after_insertion: IntGauge,
    dropped_because_late_insertion: IntGauge,
    dropped_old_age: IntGauge,
}

impl PointCloudAggregator {
    
    #[instrument(skip_all)]
    pub fn new(stream_manager: Arc<StreamManager>) -> Self {
        let metrics = get_metrics();

        Self {
            latest_point_clouds: Mutex::new(HashMap::new()),
            stream_manager,
            has_update: Mutex::new(false),
            // The maximum age of a point cloud in microseconds
            max_age: Mutex::new(5_000_000), // Currently 5 seconds
            dropped_after_insertion: metrics.get_or_create_gauge("dropped_after_insertion", "The number of point clouds that were dropped before a newer point cloud was inserted").unwrap(),
            dropped_because_late_insertion: metrics.get_or_create_gauge("dropped_because_late_insertion", "The number of point clouds that were dropped because they were older than the latest transmitted point cloud").unwrap(),
            dropped_old_age: metrics.get_or_create_gauge("dropped_old_age", "The number of point clouds that were dropped because they were too old").unwrap(),
        }
    }

    #[instrument(skip_all, fields(stream_id = %stream_id))]
    pub fn update_point_cloud(&self, stream_id: String, point_cloud: PointCloudData) {
        let mut guard = self.latest_point_clouds.lock().unwrap();
        debug!("Updating point cloud for stream {}", stream_id);
        // If the point cloud is empty, then delete the entry
        if point_cloud.points.is_empty() {
            debug!("Empty point cloud received, removing entry");
            guard.remove(&stream_id);
            return;
        // If the guard is empty, then insert the point cloud
        }

        // If buffer does not exist for this stream, create one
        let buffer = guard
            .entry(stream_id.clone())
            .or_default(); // get or create buffer

        
        // If the buffer is empty, just push and done
        // Assuming that our pipeline is fast enough, this would be the most common case and thus be O(1)
        if buffer.is_empty() {
            buffer.push_back(point_cloud);
            *self.has_update.lock().unwrap() = true;
            return;
        }

        // The buffer is not empty, we need to check where and if we should insert the point cloud
        let newest_time = buffer.back().unwrap().presentation_time;
        let oldest_time = buffer.front().unwrap().presentation_time;
        let new_time = point_cloud.presentation_time;

        // --- Optimistic O(1) checks ---
        // 1. If strictly newer than the newest (last), push back
        if new_time >= newest_time {
            debug!("New frame is >= newest_time => push_back");
            // If at capacity, discard the oldest
            if buffer.is_full() {
                self.dropped_after_insertion.inc();
                buffer.pop_front();  // Remove the oldest
            }
            buffer.push_back(point_cloud);
            *self.has_update.lock().unwrap() = true;
            return;
        }

        // 2. If strictly older than the oldest (first), push front
        if new_time <= oldest_time {
            debug!("New frame is <= oldest_time => push_front");
            // If at capacity, discard it
            if buffer.is_full() {
                // Our buffer is already full, let's discard this new frame as
                // it is older than all the other frames in the buffer
                // We assume here that the new frame is thus outdated
                self.dropped_because_late_insertion.inc();
                return;
            }
            buffer.push_front(point_cloud);
            *self.has_update.lock().unwrap() = true;
            return;
        }

        // The new frame is somewhere in the middle, we need to find the right spot.
        // We could do a binary search here, but we will just iterate through the buffer, as it is small.
        // This is case O(n) where n is the number of frames in the buffer.


        // --- Fallback: out-of-order insertion (O(n)) ---
        // 1. Extract everything into a small Vec
        let mut temp = Vec::with_capacity(buffer.len() + 1);
        while let Some(existing) = buffer.pop_front() {
            temp.push(existing);
        }

        // 2. Insert new frame in the correct position
        //    (small linear search is enough for small capacity).
        let insert_pos = temp
            .iter()
            .position(|f| new_time < f.presentation_time)
            .unwrap_or(temp.len()); // if none is bigger, insert at the end
        temp.insert(insert_pos, point_cloud);

        // 3. If over capacity, remove the truly oldest frame 
        if temp.len() > buffer.capacity() {
            self.dropped_after_insertion.inc();
            temp.remove(0); // remove front (the absolute oldest)
        }

        // 4. Push them all back in ascending order
        for frame in temp {
            buffer.push_back(frame);
        }

        // Mark that we updated
        *self.has_update.lock().unwrap() = true;
    }

    #[instrument(skip_all)]
    pub fn generate_combined_point_cloud(&self, max_number_of_points: u64) -> PointCloudData {
        let mut error_count = 0;
        
        let since_the_epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards");
        let current_time = since_the_epoch.as_micros() as u64;

        let mut guard = self.latest_point_clouds.lock().unwrap();

        // If there are no point clouds, then increment the error count and return an empty point cloud
        if guard.is_empty() {
            debug!("No point clouds to aggregate");
            return PointCloudData {
                points: Vec::new(),
                creation_time: current_time,
                presentation_time: current_time,
                error_count: 1,
            };
        }

        {
            // Check if there is an update
            if !*self.has_update.lock().unwrap() {
                // There is no update, so return an empty point cloud
                // debug!("No point cloud updates to aggregate");
                return PointCloudData {
                    points: Vec::new(),
                    creation_time: current_time,
                    presentation_time: current_time,
                    error_count: 1,
                };
            }
        }

        debug!("Aggregating point clouds");

        let max_age = *self.max_age.lock().unwrap();
        let mut max_presentation_time = 0;
        let mut latest_creation_time = 0;
        let mut streams_to_remove = Vec::new();
        let mut combined_points = Vec::new();
        let mut at_least_one_has_more_buffered = false;

        for (stream_id, buffer) in guard.iter_mut() {
            // Check if the buffer is empty, then we can schedule it for removal
            if buffer.is_empty() {
                debug!("Empty buffer received, removing entry for stream: {}", stream_id);
                streams_to_remove.push(stream_id.clone());
                continue;
            }

            // We only combine the *oldest* frame (the front of the buffer)
            // If it’s too old, pop it out and skip it.
            // (If we pop it and the buffer still has frames, subsequent calls
            // to generate_combined_point_cloud will handle them next.)
            let point_cloud = buffer.front().unwrap(); // peek the oldest

            // Check if the point cloud is empty
            if point_cloud.points.is_empty() {
                debug!("Empty point cloud received, removing entry for stream: {}", stream_id);
                streams_to_remove.push(stream_id.clone());
                continue;
            }

            // Check if the point cloud is too old (x ms after it should have been rendered)
            let overtime = current_time.saturating_sub(point_cloud.presentation_time);
            if overtime > max_age {
                debug!("Point cloud is too old, removing entry for stream: {}", stream_id);
                // Remove it from the buffer
                buffer.pop_front();
                if buffer.is_empty() {
                    streams_to_remove.push(stream_id.clone());
                }
                self.dropped_old_age.inc();
                continue;
            }

            // If we got here, the oldest frame is still valid
            // TODO: some sort of way that we can keep the selected frame in the buffer without messing up the metrics
            // That way, we can retransmit the frame if needed
            let point_cloud = buffer.pop_front().unwrap(); // consume it

            if !buffer.is_empty() {
                at_least_one_has_more_buffered = true;
            }

            // Update the max presentation time
            if point_cloud.presentation_time > max_presentation_time {
                max_presentation_time = point_cloud.presentation_time;
            }

            if point_cloud.creation_time > latest_creation_time {
                latest_creation_time = point_cloud.creation_time;
            }

            // Get the stream settings
            let settings = self.stream_manager.get_stream_settings(stream_id);

            // Apply offset and rotation
            let position = settings.position;
            let rotation = settings.rotation;            // Create scale vector
            let scale = settings.scale;

            // If all the above values are zero, then skip the transformation
            if position == [0.0, 0.0, 0.0] && rotation == [0.0, 0.0, 0.0] && scale == [1.0, 1.0, 1.0] {
                combined_points.extend_from_slice(&point_cloud.points);
                continue;
            }

            // Create rotation matrix
            let rotation_matrix = Rotation3::from_euler_angles(
                rotation[0],
                rotation[1],
                rotation[2],
            );

            // Create translation vector
            let translation = Vector3::new(position[0], position[1], position[2]);

            // Expand the capacity of the combined_points vector
            let n_points = point_cloud.points.len();
            combined_points.reserve(n_points);


            for point in &point_cloud.points {
                let scaled_point = Vector3::new(point.x * scale[0], point.y * scale[1], point.z * scale[2]);

                // Apply rotation and translation
                let transformed_point = rotation_matrix * scaled_point + translation;

                combined_points.push(Point3D {
                    x: transformed_point.x,
                    y: transformed_point.y,
                    z: transformed_point.z,
                    r: point.r,
                    g: point.g,
                    b: point.b,
                });
            }

            error_count += point_cloud.error_count;
        }

        // Remove the streams that are too old
        for stream_id in streams_to_remove {
            guard.remove(&stream_id);
        }

        if !at_least_one_has_more_buffered {
            *self.has_update.lock().unwrap() = false;
        }

        // Drop the guard here, so that the lock is released before the random sampling
        drop(guard);

        // If the number of points exceeds the maximum number of points, then randomly sample the points
        if combined_points.len() > max_number_of_points as usize {
            /*
            let rate = max_number_of_points as f64 / combined_points.len() as f64;
            combined_points = random_sampling(&combined_points, rate);
            */

            // The problem with a normal random sampling is that the points are not evenly distributed and the number of sampled points can vary
            // Which is why we use exact random sampling (which also uses a uniform distribution)
            // The speed is O(n) where n is the number of points
            combined_points = exact_random_sampling(&combined_points, max_number_of_points as usize);
        }

        PointCloudData {
            points: combined_points,
            creation_time: if latest_creation_time > 0 { latest_creation_time } else { current_time },
            presentation_time: max_presentation_time,
            error_count,
        }
    }
}
