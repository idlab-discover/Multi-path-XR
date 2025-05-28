use webrtc::rtp_transceiver::{rtp_codec::RTCRtpCodecCapability, RTCPFeedback};

pub fn video_codec_capability() -> RTCRtpCodecCapability {
    RTCRtpCodecCapability {
        mime_type:"video/pcm".to_owned(),
        clock_rate:90_000,
        channels: 0,
        sdp_fmtp_line:"".to_owned(),
        rtcp_feedback: vec![
            RTCPFeedback {
                typ: "goog-remb".to_owned(), parameter: "".to_owned(),
            },
            RTCPFeedback {
                typ: "ccm".to_owned(), parameter: "fir".to_owned(),
            },
            RTCPFeedback {
                typ: "nack".to_owned(), parameter: "".to_owned(),
            },
            RTCPFeedback {
                typ: "nack".to_owned(), parameter: "pli".to_owned(),
            }
        ],
        // ..Default::default()
    }
}