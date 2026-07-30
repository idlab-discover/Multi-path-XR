use crate::{
    diff::{DeltaCodec, DeltaKind, Point3DDelta},
    frame::{PcfFrameMeta, PcfHeader},
    types::*,
};
use spatial_codecs::encoder::{encode_into, EncodingParams};
use spatial_utils::{point::Point3D, traits::SpatialOwnedFull, utils::point_scalar::PointScalar};
use std::marker::PhantomData;

/// Policy for I/P cadence
#[derive(Clone, Copy)]
pub struct Gop {
    pub interval: u32, // emit I every N frames (N>=1)
}
impl Default for Gop {
    fn default() -> Self {
        Self { interval: 60 }
    }
}

pub struct StreamMuxer<P = Point3D, S = f32, D = Point3DDelta>
where
    D: DeltaCodec<P>,
{
    stream_id: StreamId,
    seq: SeqNo,
    last_pts: Option<u64>,
    last_frame: Vec<P>, // reconstructed (for delta)
    delta_scratch: D::Scratch,
    _scalar: PhantomData<S>,
    _delta: PhantomData<D>,
    pub delta: DeltaKind,
    pub gop: Gop,
}

pub type PointStreamMuxer = StreamMuxer<Point3D, f32, Point3DDelta>;

impl<P, S, D> StreamMuxer<P, S, D>
where
    P: SpatialOwnedFull<S> + 'static,
    S: PointScalar,
    D: DeltaCodec<P>,
{
    pub fn new(stream_id: StreamId) -> Self {
        Self {
            stream_id,
            seq: 0,
            last_pts: None,
            last_frame: Vec::new(),
            delta_scratch: D::Scratch::default(),
            _scalar: PhantomData,
            _delta: PhantomData,
            delta: DeltaKind::IndexAligned,
            gop: Gop::default(),
        }
    }

    /// Build a complete PCF frame (I or P) into `out`.
    /// `params` = inner codec to use (e.g., Draco@qp, Bitcode, TMF, …)
    pub fn write_frame(
        &mut self,
        current: &[P],
        pts_ms: Option<u64>,
        params: &EncodingParams,
        out: &mut Vec<u8>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let is_key = self.seq == 0
            || (self.gop.interval > 0 && self.seq.is_multiple_of(self.gop.interval as u64));
        let use_delta =
            !is_key && matches!(self.delta, DeltaKind::IndexAligned) && !self.last_frame.is_empty();

        let residuals = if use_delta {
            D::compute_residuals_index_aligned(&self.last_frame, current, &mut self.delta_scratch)
        } else {
            None
        };

        // choose what to encode (absolute vs residual)
        let (key, delta, ref_seq, to_encode): (bool, bool, Option<u64>, &[P]) =
            if let Some(deltas) = residuals {
                (false, true, Some(self.seq - 1), deltas)
            } else {
                (true, false, None, current)
            };

        // inner encode into a temp to read its 3-byte magic
        let mut payload = Vec::new();
        encode_into::<P, S>(to_encode, params, &mut payload)?;
        let codec_magic = [payload[0], payload[1], payload[2]];

        // write header then payload in one go (no second buffer)
        let meta = PcfFrameMeta {
            key,
            delta,
            codec_magic: Some(codec_magic),
            stream_id: Some(self.stream_id),
            seq: Some(self.seq),
            presentation_time_us: pts_ms,
            ref_seq,
            ..Default::default()
        };
        PcfHeader::write_frame_to(out, &meta, &payload)?;

        // update state: store reconstructed for delta (we keep full current for next step)
        self.last_frame.clear();
        self.last_frame.extend_from_slice(current);
        self.seq = self.seq.wrapping_add(1);
        self.last_pts = pts_ms;
        Ok(())
    }
}
