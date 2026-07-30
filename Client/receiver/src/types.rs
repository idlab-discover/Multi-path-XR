use std::sync::Arc;

use shared_utils::types::FrameData;

pub type DataCallback = Arc<dyn Fn(FrameData, String) + Send + Sync>;
