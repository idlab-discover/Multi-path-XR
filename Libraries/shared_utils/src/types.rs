use std::time::{SystemTime, UNIX_EPOCH};

use bitcode::{Encode as EncodeBitcode, Decode as DecodeBitcode};
use serde::{Deserialize, Serialize};
use ply_rs::ply::{Property, PropertyAccess};
use tracing::warn;

#[derive(Clone, Debug, Deserialize, Serialize, EncodeBitcode, DecodeBitcode, PartialEq, Default)]
pub struct Point3D {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub r: u8,
    pub g: u8,
    pub b: u8,
}


impl PropertyAccess for Point3D {
    fn new() -> Self {
        // Return a default/zeroed-out struct
        Point3D::default()
    }

    fn set_property(&mut self, key: &str, property: Property) {
        match (key, property) {
            ("x", Property::Float(v)) => self.x = v,
            ("y", Property::Float(v)) => self.y = v,
            ("z", Property::Float(v)) => self.z = v,
            ("x", Property::Double(v)) => self.x = v as f32,
            ("y", Property::Double(v)) => self.y = v as f32,
            ("z", Property::Double(v)) => self.z = v as f32,
            ("red", Property::UChar(v)) => self.r = v,
            ("green", Property::UChar(v)) => self.g = v,
            ("blue", Property::UChar(v)) => self.b = v,
            // Possibly handle other property types or names, e.g. "Property::Float"
            (k, _) => warn!("Ignoring unexpected key or property type: {}", k),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, EncodeBitcode, DecodeBitcode)]
pub struct FrameTaskData {
    pub send_time: u64,
    pub presentation_time: u64,
    pub data: Vec<u8>,
    // fields for SFU usage
    // (all optional so existing code can ignore them)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sfu_client_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sfu_frame_len: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sfu_tile_index: Option<u32>,
}

// Implement PartialEq for FrameTaskData
impl PartialEq for FrameTaskData {
    fn eq(&self, other: &Self) -> bool {
            self.presentation_time == other.presentation_time
            && self.sfu_client_id.is_none_or(|cid| other.sfu_client_id.is_none_or(|other_cid| cid == other_cid))
            && self.sfu_tile_index.is_none_or(|ti| other.sfu_tile_index.is_none_or(|other_ti| ti == other_ti))
            && self.sfu_frame_len.is_none_or(|fl| other.sfu_frame_len.is_none_or(|other_fl| fl == other_fl))
            && self.data == other.data
            // We ignore the send time in the comparison
    }
}

// Implement PartialOrd for FrameTaskData
// based on presentation_time, when those are equal, compare based on send_time
impl PartialOrd for FrameTaskData {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        match self.presentation_time.cmp(&other.presentation_time) {
            std::cmp::Ordering::Equal => self.send_time.partial_cmp(&other.send_time),
            other => Some(other),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PointCloudData {
    pub points: Vec<Point3D>,
    pub creation_time: u64,
    pub presentation_time: u64,
    pub error_count: u64,
}

// Implement the default trait for PointCloudData
impl Default for PointCloudData {
    fn default() -> Self {
        // Get the current time
        let since_the_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards");
        let current_time = since_the_epoch.as_micros() as u64;
        let presentation_tim_offset = 100_000; // microseconds after creation time

        Self {
            points: Vec::new(),
            creation_time: current_time,
            presentation_time: current_time + presentation_tim_offset,
            error_count: 0,
        }
    }
}