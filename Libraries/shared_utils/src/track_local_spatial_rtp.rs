use async_trait::async_trait;
use std::sync::Mutex;
use tracing::instrument;
use webrtc::error::flatten_errs;
use webrtc::rtp::packetizer::{new_packetizer, Packetizer};
use webrtc::rtp::sequence::{new_random_sequencer, Sequencer};
use webrtc::rtp_transceiver::rtp_codec::{
    RTCRtpCodecCapability, RTCRtpCodecParameters, RTPCodecType,
};
use webrtc::track::track_local::track_local_static_rtp::TrackLocalStaticRTP;
use webrtc::track::track_local::{TrackLocal, TrackLocalContext};
// use std::time::Instant;

use crate::types::FrameTaskData;

use super::spatial_payloader::SpatialRtpPayloader;

#[derive(Debug)]
struct TrackLocalSpatialRtpInternal {
    packetizer: Option<Box<dyn Packetizer + Send + Sync>>,
    sequencer: Option<Box<dyn Sequencer + Send + Sync>>,
    payloader: SpatialRtpPayloader,
    clock_rate: f64,
    fps: f64,
}

pub struct TrackLocalSpatialRtp {
    rtp_track: TrackLocalStaticRTP,
    internal: Mutex<TrackLocalSpatialRtpInternal>,
}

impl TrackLocalSpatialRtp {
    #[instrument(skip_all)]
    pub fn new(codec: RTCRtpCodecCapability, id: String, stream_id: String, fps: u32) -> Self {
        // 1) Create the underlying track
        let rtp_track = TrackLocalStaticRTP::new(codec.clone(), id, stream_id);

        // 2) Create our internal state
        let internal = TrackLocalSpatialRtpInternal {
            packetizer: None, // Packetizer will be initialized in `bind()`
            sequencer: None,
            payloader: SpatialRtpPayloader::new(),
            clock_rate: codec.clock_rate as f64, // This
            fps: fps as f64,
        };

        Self {
            rtp_track,
            internal: Mutex::new(internal),
        }
    }

    #[instrument(skip_all)]
    pub fn new_with_rid(
        codec: RTCRtpCodecCapability,
        id: String,
        rid: String,
        stream_id: String,
        fps: u32,
    ) -> Self {
        // 1) Create the underlying track
        let track_local_static =
            TrackLocalStaticRTP::new_with_rid(codec.clone(), id, rid, stream_id);

        // 2) Create our internal state
        let internal = TrackLocalSpatialRtpInternal {
            packetizer: None, // Packetizer will be initialized in `bind()`
            sequencer: None,
            payloader: SpatialRtpPayloader::new(),
            clock_rate: codec.clock_rate as f64,
            fps: fps as f64,
        };

        Self {
            rtp_track: track_local_static,
            internal: Mutex::new(internal),
        }
    }

    /// codec gets the Codec of the track
    pub fn codec(&self) -> RTCRtpCodecCapability {
        self.rtp_track.codec()
    }

    /// write_frame writes a frame to the track
    ///
    #[instrument(skip_all)]
    pub async fn write_frame(&self, frame: &FrameTaskData) -> Result<(), webrtc::Error> {
        self.write_payload(&frame.data, frame.send_time).await
    }

    #[instrument(skip_all)]
    pub async fn write_payload(&self, payload: &[u8], frame_nr: u64) -> Result<(), webrtc::Error> {
        let raw_payload = bytes::Bytes::copy_from_slice(payload);

        // 2) Convert the frame into a vector of packets
        let packets = {
            // 2.0) Lock the internal state, otherwise we might corrupt the data when we write multiple frames at the same time
            let mut internal = self.internal.lock().unwrap();

            // 2.1) Extract payloader into a separate scope to avoid multiple mutable borrows
            {
                let mut payloader = internal.payloader.clone();
                payloader.set_frame_nr(frame_nr);
            }

            // 2.2) “samples” is how many clock ticks we’re generating in one frame
            let samples = 0; /*{
                                 (internal.clock_rate / internal.fps) as u32
                             };*/
            // TODO: sample duration * clock rate is what pion and webrtc do in track_local_static
            // https://github.com/pion/webrtc/blob/master/track_local_static.go#L343
            // https://github.com/webrtc-rs/webrtc/blob/master/webrtc/src/track/track_local/track_local_static_sample.rs#L137

            // 2.3) Ensure packetizer is initialized
            let mut packetizer = if let Some(packetizer) = internal.packetizer.as_mut() {
                packetizer.clone()
            } else {
                return Err(webrtc::Error::new(
                    "Packetizer is not initialized. Call `bind()` first.".to_owned(),
                ));
            };

            // TODO: check if we should implement the prev_dropped_packets from the links above

            // 2.4) Packetize
            packetizer.packetize(&raw_payload, samples)
        };

        // 3) If the packetizer returns an error, return it
        let packets = match packets {
            Ok(p) => p,
            Err(e) => return Err(webrtc::Error::from(e)),
        };

        // Start a timer to measure the time taken for packetization
        //let start_time = Instant::now();

        // info!("Packetized {} packets", packets.len());

        // 4) Write each packet
        let mut write_errs = vec![];
        for p in packets {
            if let Err(err) = self.rtp_track.write_rtp_with_extensions(&p, &[]).await {
                write_errs.push(err);
            }
        }

        // Measure the time taken for packetization
        // let elapsed_time = start_time.elapsed();
        // info!("Sending all packets for this frame took took: {:?} ms", elapsed_time.as_millis());

        flatten_errs(write_errs)
    }

    #[instrument(skip_all)]
    pub fn set_fps(&self, fps: u32) {
        let mut internal = self.internal.lock().unwrap();
        internal.fps = fps as f64;
    }
}

// Implement the required trait for track binding/unbinding
#[async_trait]
impl TrackLocal for TrackLocalSpatialRtp {
    #[instrument(skip_all)]
    async fn bind(&self, ctx: &TrackLocalContext) -> Result<RTCRtpCodecParameters, webrtc::Error> {
        let codec = self.rtp_track.bind(ctx).await?;
        let mut internal = self.internal.lock().unwrap();

        // If packetizer is already initialized, return codec
        if internal.packetizer.is_some() {
            return Ok(codec);
        }

        let payloader = Box::new(internal.payloader.clone());
        let sequencer: Box<dyn Sequencer + Send + Sync> = Box::new(new_random_sequencer());

        internal.packetizer = Some(Box::new(new_packetizer(
            1200,               // Max packet size
            codec.payload_type, // Payload type (set by SDP negotiation)
            ctx.ssrc(),         // SSRC assigned by WebRTC
            payloader,
            sequencer.clone(),
            codec.capability.clock_rate,
        )));
        internal.sequencer = Some(sequencer);
        internal.clock_rate = codec.capability.clock_rate as f64;

        Ok(codec)
    }

    #[instrument(skip_all)]
    async fn unbind(&self, ctx: &TrackLocalContext) -> Result<(), webrtc::Error> {
        self.rtp_track.unbind(ctx).await
    }

    #[instrument(skip_all)]
    fn id(&self) -> &str {
        self.rtp_track.id()
    }

    /// RID is the RTP Stream ID for this track.
    #[instrument(skip_all)]
    fn rid(&self) -> Option<&str> {
        self.rtp_track.rid()
    }

    #[instrument(skip_all)]
    fn stream_id(&self) -> &str {
        self.rtp_track.stream_id()
    }

    #[instrument(skip_all)]
    fn kind(&self) -> RTPCodecType {
        self.rtp_track.kind()
    }

    #[instrument(skip_all)]
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
