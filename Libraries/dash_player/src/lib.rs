pub mod mpd;
pub mod segment;
pub mod player;
use bytes::Bytes;

/// Events emitted by the player
pub enum DashEvent {
    Segment {
        data: Bytes,
        content_type: String,
        representation_id: String,
        segment_number: u64,
        duration: f64,
        url: String,
        playback_rate: f64,
    },
    DownloadError {
        url: String,
        reason: String,
    },
    Info(String),
    Warning(String),
}


pub use player::DashPlayer;
