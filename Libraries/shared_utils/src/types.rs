use std::time::{SystemTime, UNIX_EPOCH};

use spatial_utils::point::Point3D;
use spatial_utils::splat::GaussianSplatF32;

use serde::{Deserialize, Serialize};

pub type BasicResult = Result<(), Box<dyn std::error::Error>>;

#[repr(u8)]
#[derive(Copy, Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub enum FramePayloadContainer {
    #[default]
    Raw = 0,
    Pcf = 1,
}

#[repr(u8)]
#[derive(Copy, Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub enum FrameRenderPrimitive {
    #[default]
    Points = 0,
    GaussianSplats = 1,
}

#[derive(Copy, Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct FramePayloadMetadata {
    pub container: FramePayloadContainer,
    pub primitive: FrameRenderPrimitive,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct FrameTaskData {
    pub send_time: u64,
    pub presentation_time: u64,
    pub payload_metadata: FramePayloadMetadata,
    pub data: Vec<u8>,
    // fields for SFU/Relay/DASH usage
    // (all optional so existing code can ignore them)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality_index: Option<u32>,
}

// Implement PartialEq for FrameTaskData
impl PartialEq for FrameTaskData {
    fn eq(&self, other: &Self) -> bool {
        self.presentation_time == other.presentation_time
            && self
                .client_id
                .is_none_or(|cid| other.client_id.is_none_or(|other_cid| cid == other_cid))
            && self
                .quality_index
                .is_none_or(|ti| other.quality_index.is_none_or(|other_ti| ti == other_ti))
            && self.payload_metadata == other.payload_metadata
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

#[derive(Clone)]
pub struct FrameData {
    pub send_time: u64,
    pub presentation_time: u64,
    pub receive_time: u64,
    pub quality_index: Option<u32>,
    pub render_primitive: FrameRenderPrimitive,
    pub error_count: u64,
    pub point_count: u64,
    pub coordinates: Vec<f32>,
    pub colors: Vec<u8>,
    pub gaussian_scales: Vec<f32>,
    pub gaussian_rotations: Vec<f32>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum SpatialPayload {
    Points(Vec<Point3D>),
    GaussianSplats(Vec<GaussianSplatF32>),
}

impl SpatialPayload {
    #[inline]
    pub fn primitive(&self) -> FrameRenderPrimitive {
        match self {
            Self::Points(_) => FrameRenderPrimitive::Points,
            Self::GaussianSplats(_) => FrameRenderPrimitive::GaussianSplats,
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        match self {
            Self::Points(points) => points.len(),
            Self::GaussianSplats(splats) => splats.len(),
        }
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SpatialFrameData {
    pub payload: SpatialPayload,
    pub creation_time: u64,
    pub presentation_time: u64,
    pub error_count: u64,
}

impl SpatialFrameData {
    #[inline]
    pub fn render_primitive(&self) -> FrameRenderPrimitive {
        self.payload.primitive()
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.payload.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.payload.is_empty()
    }
}

impl Default for SpatialFrameData {
    fn default() -> Self {
        let since_the_epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards");
        let current_time = since_the_epoch.as_micros() as u64;
        let presentation_time_offset = 100_000;

        Self {
            payload: SpatialPayload::Points(Vec::new()),
            creation_time: current_time,
            presentation_time: current_time + presentation_time_offset,
            error_count: 0,
        }
    }
}
