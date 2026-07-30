use crate::{
    diff::{DeltaCodec, Point3DDelta},
    frame::PcfHeader,
    types::*,
};
use spatial_codecs::decoder::decode_into;
use spatial_utils::{point::Point3D, traits::SpatialSink};
use std::collections::HashMap;
use std::marker::PhantomData;

pub struct StreamState<P = Point3D> {
    last_frame: Vec<P>,
}

impl<P> Default for StreamState<P> {
    fn default() -> Self {
        Self {
            last_frame: Vec::new(),
        }
    }
}

pub struct Demuxer<P = Point3D, D = Point3DDelta>
where
    D: DeltaCodec<P>,
{
    streams: HashMap<StreamId, StreamState<P>>,
    _delta: PhantomData<D>,
}

pub type PointDemuxer = Demuxer<Point3D, Point3DDelta>;

type DemuxerResult<P> = Result<(StreamId, SeqNo, Option<u64>, Vec<P>), Box<dyn std::error::Error>>;

impl<P, D> Demuxer<P, D>
where
    P: SpatialSink + 'static,
    D: DeltaCodec<P>,
{
    pub fn new() -> Self {
        Self {
            streams: HashMap::new(),
            _delta: PhantomData,
        }
    }

    /// Feed one full PCF frame (already reassembled if chunked).
    /// Returns reconstructed frame.
    pub fn push_frame(&mut self, data: &[u8]) -> DemuxerResult<P> {
        let hdr = PcfHeader::parse(data).map_err(|e| format!("{}", e))?;
        let stream_id = hdr.stream_id.unwrap_or(0);
        let seq = hdr.seq.unwrap_or(0);
        let st = self.streams.entry(stream_id).or_default();

        // Decode payload first
        let mut decoded = Vec::new();
        decode_into(hdr.payload, &mut decoded)?; // payload begins with inner magic already

        // If delta, apply onto previous
        if hdr.flags.contains(Flags::DELTA) {
            if st.last_frame.is_empty() {
                return Err("delta without reference".into());
            }
            // Work on a copy to keep last_frame intact if desired; here we mutate into next frame.
            let mut base = st.last_frame.clone();
            D::apply_residuals_index_aligned(&mut base, &decoded)?;
            st.last_frame = base.clone();
            Ok((stream_id, seq, hdr.presentation_time_us, base))
        } else {
            st.last_frame = decoded.clone();
            Ok((stream_id, seq, hdr.presentation_time_us, decoded))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{diff::NoDelta, frame::PcfHeader, muxer::StreamMuxer};
    use spatial_codecs::encoder::{get_default_params, EncodingFormat};
    use spatial_utils::{color::Rgba8, splat::GaussianSplatF32};

    #[test]
    fn round_trips_gaussian_splat_i_frame() {
        let splats = vec![
            GaussianSplatF32::new(
                [1.0, 2.0, 3.0],
                Rgba8::new(10, 20, 30, 255),
                [0.1, 0.2, 0.3],
                [1.0, 0.0, 0.0, 0.0],
            ),
            GaussianSplatF32::new(
                [4.0, 5.0, 6.0],
                Rgba8::new(40, 50, 60, 128),
                [0.4, 0.5, 0.6],
                [0.0, 1.0, 0.0, 0.0],
            ),
        ];
        let params = get_default_params(EncodingFormat::Bitcode);
        let mut muxer = StreamMuxer::<GaussianSplatF32, f32, NoDelta>::new(7);
        let mut frame = Vec::new();

        muxer
            .write_frame(&splats, Some(123), &params, &mut frame)
            .unwrap();

        let header = PcfHeader::parse(&frame).unwrap();
        assert_eq!(header.stream_id, Some(7));
        assert_eq!(header.presentation_time_us, Some(123));
        assert_eq!(
            PcfHeader::inner_codec_magic(&frame).unwrap(),
            header.codec_magic.unwrap()
        );
        assert_eq!(&header.payload[..3], &header.codec_magic.unwrap());

        let mut demuxer = Demuxer::<GaussianSplatF32, NoDelta>::new();
        let (stream_id, _seq, pts_ms, decoded) = demuxer.push_frame(&frame).unwrap();

        assert_eq!(stream_id, 7);
        assert_eq!(pts_ms, Some(123));
        assert_eq!(decoded, splats);
    }
}
