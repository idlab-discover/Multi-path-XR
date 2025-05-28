//! DASH manifest data structures (MPD and related types).
//! These represent parsed MPEG-DASH metadata including segment timing and adaptation sets.

pub mod parser;
pub mod builder;

use std::collections::HashMap;
use chrono::{DateTime, Utc};

/// A single video/audio representation within an adaptation set.
#[derive(Debug, Clone)]
pub struct Representation {
    /// Unique identifier for the representation.
    pub id: String,
    /// Average bandwidth in bits per second (bps).
    pub bandwidth: u64,
    /// URL template for the initialization segment.
    pub initialization: String,
    /// URL template for the media segments (may contain $Number$, $Time$, etc.).
    pub media: String,
    /// Duration of each segment in seconds. Derived from `duration / timescale` in SegmentTemplate.
    pub segment_duration: f64,
    /// Timescale used to convert segment timing to seconds. E.g., `timescale=1000000` means 1 unit = 1 microsecond.
    pub timescale: u64,
    /// Whether the segment addressing is based on $Time$ instead of $Number$.
    pub uses_segment_time: bool,
    /// True if a usable SegmentTemplate was resolved for this representation.
    pub has_template: bool,
    pub availability_time_offset: Option<f64>,
    pub availability_time_complete: Option<bool>,
    pub presentation_time_offset: Option<u64>,
    pub segment_timeline: Option<Vec<(u64, u64)>>,
}

/// An adaptation set groups representations with the same content type (e.g., audio or video).
#[derive(Debug, Clone)]
pub struct AdaptationSet {
    /// Content type of the adaptation set (e.g., "audio" or "video").
    pub content_type: String,
    /// MIME type of the media (e.g., "video/mp4").
    pub mime_type: String,
    /// All representations available in this adaptation set.
    pub representations: Vec<Representation>,
    /// Optional SegmentTemplate attributes defined at the AdaptationSet level.
    pub segment_template: Option<HashMap<String, String>>,
}

/// Top-level metadata parsed from an MPD file.
#[derive(Debug, Clone)]
pub struct MpdMetadata {
    /// The wall-clock time when the presentation became available (used to calculate live edge).
    pub availability_start_time: DateTime<Utc>,
    /// The wall-clock time when the presentation ends (used to calculate live edge).
    pub time_shift_buffer_depth: Option<f64>,
    /// All adaptation sets (audio/video tracks) in the current Period.
    pub adaptation_sets: Vec<AdaptationSet>,
}
