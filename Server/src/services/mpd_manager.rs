// Server/src/egress/mpd_manager.rs

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use chrono::Utc;
use dash_player::mpd::builder::{MpdBuilder, RepresentationDef};

#[derive(Clone)]
pub struct MpdManager {
    pub builders: Arc<Mutex<HashMap<String, MpdBuilder>>>,
    notify_new_group: Option<Arc<dyn Fn(String) + Send + Sync>>,
}

impl std::fmt::Debug for MpdManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MpdManager")
            .field("notify_new_group", &"<callback>")
            .field("builders", &self.builders.lock().unwrap().len())
            .finish()
    }
}

impl MpdManager {
    pub fn new() -> Self {
        Self {
            builders: Arc::new(Mutex::new(HashMap::new())),
            notify_new_group: None,
        }
    }

    pub fn set_notify_callback(&mut self, callback: Arc<dyn Fn(String) + Send + Sync>) {
        self.notify_new_group = Some(callback);
    }

    pub fn add_stream_to_mpd(
        &self,
        group_id: &str,
        stream_id: &str,
        mime_type: &str,
        codecs: &str,
        bandwidth: u64,
        fps: u64
    ) {
        let mut builders = self.builders.lock().unwrap();
        let builder = builders.entry(group_id.to_string()).or_insert_with(|| {
            MpdBuilder::live()
                .availability_start(Utc::now() - chrono::Duration::milliseconds(124))
                .time_shift_buffer(0.2)
                .segment_duration(1_000, fps * 1_000)
                .minimum_update_period(60.0)
                .suggested_presentation_delay(0.030)
        });

        let representation_exists = builder.representations.iter().any(|r| r.id == stream_id);
        if !representation_exists {
            builder.representations.push(RepresentationDef {
                id: stream_id.to_string(),
                mime_type: mime_type.to_string(),
                codecs: codecs.to_string(),
                bandwidth,
                initialization: format!("{}/init.mp4", stream_id),
                media: format!("{}/$Number%09d$.m4s", stream_id),
                availability_time_offset: Some(-0.030),
                availability_time_complete: Some(false)
            });
        }

        if let Some(callback) = &self.notify_new_group {
            (callback)(group_id.to_string());
        }
    }

    pub fn get_mpd(&self, group_id: &str) -> Option<String> {
        let builders = self.builders.lock().unwrap();
        builders.get(group_id).and_then(|b| b.build_xml_string().ok())
    }

    pub fn get_groups(&self) -> Vec<String> {
        let builders = self.builders.lock().unwrap();
        builders.keys().cloned().collect()
    }
}

