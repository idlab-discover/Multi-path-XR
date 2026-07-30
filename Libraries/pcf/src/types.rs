use bitflags::bitflags;

pub type StreamId = u32;
pub type SeqNo = u64;

pub const PCF_MAGIC: &[u8; 3] = b"PCF";
pub const PCF_VERSION: u8 = 2;
pub const PCF_BASE_HEADER_LEN: usize = 3 + 1 + 1 + 2;

bitflags! {
    #[repr(C)]
    #[derive(Debug, Clone)]
    pub struct Flags: u16 {
        const KEY                   = 1 << 0;  // I-frame
        const DELTA                 = 1 << 1;  // P-frame (payload are residuals)
        const HAS_CODEC_MAGIC       = 1 << 2;
        const HAS_STREAM_ID         = 1 << 3;
        const HAS_SEQ               = 1 << 4;
        const HAS_SEND_TIME         = 1 << 5;
        const HAS_PRESENTATION_TIME = 1 << 6;
        const HAS_REF_SEQ           = 1 << 7;
        const HAS_CLIENT_ID         = 1 << 8;
        const HAS_QUALITY_INDEX     = 1 << 9;
        const HAS_RENDER_PRIMITIVE  = 1 << 10;
        const HAS_PAYLOAD_LEN       = 1 << 11;
    }
}

#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RenderPrimitive {
    Points = 0,
    GaussianSplats = 1,
}

impl RenderPrimitive {
    #[inline]
    pub fn from_u8(raw: u8) -> Option<Self> {
        match raw {
            0 => Some(Self::Points),
            1 => Some(Self::GaussianSplats),
            _ => None,
        }
    }
}

/// Minimal error without dependencies.
#[repr(C)]
#[derive(Debug)]
pub struct PcfError(pub &'static str);
impl core::fmt::Display for PcfError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl std::error::Error for PcfError {}

#[inline]
pub fn le_u32(x: u32) -> [u8; 4] {
    x.to_le_bytes()
}
#[inline]
pub fn le_u64(x: u64) -> [u8; 8] {
    x.to_le_bytes()
}
