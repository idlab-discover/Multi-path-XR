use std::sync::Arc;

use webrtc::{api::{interceptor_registry::{configure_nack, configure_rtcp_reports, configure_twcc}, media_engine::MediaEngine, APIBuilder}, ice_transport::ice_server::RTCIceServer, interceptor::registry::Registry, peer_connection::{configuration::RTCConfiguration, RTCPeerConnection}, rtp_transceiver::rtp_codec::{RTCRtpCodecParameters, RTPCodecType}};

use crate::codec::video_codec_capability;

pub async fn create_webrtc_peer_connection() -> Result<Arc<RTCPeerConnection>, String> {
    let mut m = MediaEngine::default();
    m.register_default_codecs().unwrap();
    let _ = m.register_codec(RTCRtpCodecParameters {
        capability: video_codec_capability(),
        payload_type: 5,
        ..Default::default()
    }, RTPCodecType::Video);
    let mut registry = Registry::new();
    registry = configure_nack(registry, &mut m);
    registry = configure_rtcp_reports(registry);
    registry = configure_twcc(registry, &mut m).map_err(|e| format!("configure_twcc failed: {e}"))?;
    // registry = register_default_interceptors(registry, &mut m).unwrap();
    let api = APIBuilder::new()
        .with_media_engine(m)
        .with_interceptor_registry(registry)
        .build();

    let config = RTCConfiguration {
        ice_servers: vec![RTCIceServer {
            urls: vec!["stun:stun.l.google.com:19302".to_owned()],
            ..Default::default()
        }],
        ..Default::default()
    };
    Ok(Arc::new(
        api.new_peer_connection(config)
            .await
            .map_err(|e| format!("new_peer_connection failed: {e}"))?,
    ))
}