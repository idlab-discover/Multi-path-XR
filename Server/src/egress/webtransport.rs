// egress/protocols/webtransport.rs
// (WebTransport supports prioritization)
use crate::encoders::EncodingFormat;
use crate::protocols::traits::EgressProtocol;
use webtransport::session::WebTransportSession;
use std::sync::Arc;

use super::egress_common::EgressProtocol;

#[derive(Clone, Debug)]
pub struct WebTransportEgress {
    session: Arc<WebTransportSession>,
    priority: u8,
}

impl WebTransportEgress {
    pub fn new(session: Arc<WebTransportSession>) -> Self {
        Self { session, priority: 0 }
    }
}

impl EgressProtocol for WebTransportEgress {
    #[inline]
    fn encoding_format(&self) -> EncodingFormat {
        *self.encoding_format.lock().unwrap()
    }

    #[inline]
    fn max_number_of_points(&self) -> u64 {
        *self.max_number_of_points.lock().unwrap()
    }
}