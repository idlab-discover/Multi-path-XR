use std::sync::Arc;

pub type DataCallback = Arc<dyn Fn(FrameData, String) + Send + Sync>;

#[derive(Clone)]
pub struct FrameData {
    pub send_time: u64,
    pub presentation_time: u64,
    pub receive_time: u64,
    pub error_count: u64,
    pub point_count: u64,
    pub coordinates: Vec<f32>,
    pub colors: Vec<u8>,
}