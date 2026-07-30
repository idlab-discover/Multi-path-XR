use crate::types::*;

#[repr(C)]
#[derive(Debug, Clone, Default)]
pub struct PcfFrameMeta {
    pub key: bool,
    pub delta: bool,
    pub codec_magic: Option<[u8; 3]>,
    pub stream_id: Option<StreamId>,
    pub seq: Option<SeqNo>,
    pub send_time_us: Option<u64>,
    pub presentation_time_us: Option<u64>,
    pub ref_seq: Option<SeqNo>,
    pub client_id: Option<u64>,
    pub quality_index: Option<u32>,
    pub render_primitive: Option<RenderPrimitive>,
    pub payload_len: Option<u32>,
}

impl PcfFrameMeta {
    pub fn flags(&self) -> Flags {
        let mut flags = Flags::empty();
        if self.key {
            flags |= Flags::KEY;
        }
        if self.delta {
            flags |= Flags::DELTA;
        }
        if self.codec_magic.is_some() {
            flags |= Flags::HAS_CODEC_MAGIC;
        }
        if self.stream_id.is_some() {
            flags |= Flags::HAS_STREAM_ID;
        }
        if self.seq.is_some() {
            flags |= Flags::HAS_SEQ;
        }
        if self.send_time_us.is_some() {
            flags |= Flags::HAS_SEND_TIME;
        }
        if self.presentation_time_us.is_some() {
            flags |= Flags::HAS_PRESENTATION_TIME;
        }
        if self.ref_seq.is_some() {
            flags |= Flags::HAS_REF_SEQ;
        }
        if self.client_id.is_some() {
            flags |= Flags::HAS_CLIENT_ID;
        }
        if self.quality_index.is_some() {
            flags |= Flags::HAS_QUALITY_INDEX;
        }
        if self.render_primitive.is_some() {
            flags |= Flags::HAS_RENDER_PRIMITIVE;
        }
        if self.payload_len.is_some() {
            flags |= Flags::HAS_PAYLOAD_LEN;
        }
        flags
    }

    fn header_len(&self) -> usize {
        PCF_BASE_HEADER_LEN
            + self.codec_magic.map_or(0, |_| 3)
            + self.stream_id.map_or(0, |_| 4)
            + self.seq.map_or(0, |_| 8)
            + self.send_time_us.map_or(0, |_| 8)
            + self.presentation_time_us.map_or(0, |_| 8)
            + self.ref_seq.map_or(0, |_| 8)
            + self.client_id.map_or(0, |_| 8)
            + self.quality_index.map_or(0, |_| 4)
            + self.render_primitive.map_or(0, |_| 1)
            + self.payload_len.map_or(0, |_| 4)
    }
}

#[repr(C)]
#[derive(Debug, Clone)]
pub struct PcfHeader<'a> {
    pub flags: Flags,
    pub codec_magic: Option<[u8; 3]>,
    pub stream_id: Option<StreamId>,
    pub seq: Option<SeqNo>,
    pub send_time_us: Option<u64>,
    pub presentation_time_us: Option<u64>,
    pub ref_seq: Option<SeqNo>,
    pub client_id: Option<u64>,
    pub quality_index: Option<u32>,
    pub render_primitive: Option<RenderPrimitive>,
    pub payload: &'a [u8],
}

impl<'a> PcfHeader<'a> {
    pub fn write_header_to(out: &mut Vec<u8>, meta: &PcfFrameMeta) -> Result<(), PcfError> {
        let header_len = meta.header_len();
        if header_len > u8::MAX as usize {
            return Err(PcfError("pcf: header too large"));
        }

        out.reserve(header_len + meta.payload_len.unwrap_or(0) as usize);
        out.extend_from_slice(PCF_MAGIC);
        out.push(PCF_VERSION);
        out.push(header_len as u8);
        out.extend_from_slice(&meta.flags().bits().to_le_bytes());
        if let Some(codec_magic) = meta.codec_magic {
            out.extend_from_slice(&codec_magic);
        }
        if let Some(stream_id) = meta.stream_id {
            out.extend_from_slice(&le_u32(stream_id));
        }
        if let Some(seq) = meta.seq {
            out.extend_from_slice(&le_u64(seq));
        }
        if let Some(send_time_us) = meta.send_time_us {
            out.extend_from_slice(&le_u64(send_time_us));
        }
        if let Some(presentation_time_us) = meta.presentation_time_us {
            out.extend_from_slice(&le_u64(presentation_time_us));
        }
        if let Some(ref_seq) = meta.ref_seq {
            out.extend_from_slice(&le_u64(ref_seq));
        }
        if let Some(client_id) = meta.client_id {
            out.extend_from_slice(&le_u64(client_id));
        }
        if let Some(quality_index) = meta.quality_index {
            out.extend_from_slice(&le_u32(quality_index));
        }
        if let Some(render_primitive) = meta.render_primitive {
            out.push(render_primitive as u8);
        }
        if let Some(payload_len) = meta.payload_len {
            out.extend_from_slice(&le_u32(payload_len));
        }
        Ok(())
    }

    pub fn write_frame_to(
        out: &mut Vec<u8>,
        meta: &PcfFrameMeta,
        payload: &[u8],
    ) -> Result<(), PcfError> {
        Self::write_header_to(out, meta)?;
        out.extend_from_slice(payload);
        Ok(())
    }

    pub fn parse(data: &'a [u8]) -> Result<PcfHeader<'a>, PcfError> {
        if data.len() < PCF_BASE_HEADER_LEN {
            return Err(PcfError("pcf: truncated"));
        }
        if &data[0..3] != PCF_MAGIC {
            return Err(PcfError("pcf: bad magic"));
        }
        if data[3] != PCF_VERSION {
            return Err(PcfError("pcf: unsupported version"));
        }

        let header_len = data[4] as usize;
        if header_len < PCF_BASE_HEADER_LEN || data.len() < header_len {
            return Err(PcfError("pcf: truncated header"));
        }

        let flags = Flags::from_bits_truncate(u16::from_le_bytes([data[5], data[6]]));
        let mut offset = PCF_BASE_HEADER_LEN;

        let codec_magic = if flags.contains(Flags::HAS_CODEC_MAGIC) {
            let bytes = read_exact(data, &mut offset, header_len, 3)?;
            Some([bytes[0], bytes[1], bytes[2]])
        } else {
            None
        };
        let stream_id = if flags.contains(Flags::HAS_STREAM_ID) {
            Some(read_u32(data, &mut offset, header_len)?)
        } else {
            None
        };
        let seq = if flags.contains(Flags::HAS_SEQ) {
            Some(read_u64(data, &mut offset, header_len)?)
        } else {
            None
        };
        let send_time_us = if flags.contains(Flags::HAS_SEND_TIME) {
            Some(read_u64(data, &mut offset, header_len)?)
        } else {
            None
        };
        let presentation_time_us = if flags.contains(Flags::HAS_PRESENTATION_TIME) {
            Some(read_u64(data, &mut offset, header_len)?)
        } else {
            None
        };
        let ref_seq = if flags.contains(Flags::HAS_REF_SEQ) {
            Some(read_u64(data, &mut offset, header_len)?)
        } else {
            None
        };
        let client_id = if flags.contains(Flags::HAS_CLIENT_ID) {
            Some(read_u64(data, &mut offset, header_len)?)
        } else {
            None
        };
        let quality_index = if flags.contains(Flags::HAS_QUALITY_INDEX) {
            Some(read_u32(data, &mut offset, header_len)?)
        } else {
            None
        };
        let render_primitive = if flags.contains(Flags::HAS_RENDER_PRIMITIVE) {
            let bytes = read_exact(data, &mut offset, header_len, 1)?;
            Some(RenderPrimitive::from_u8(bytes[0]).ok_or(PcfError("pcf: bad render primitive"))?)
        } else {
            None
        };
        let payload_len = if flags.contains(Flags::HAS_PAYLOAD_LEN) {
            Some(read_u32(data, &mut offset, header_len)? as usize)
        } else {
            None
        };

        if offset != header_len {
            return Err(PcfError("pcf: header length mismatch"));
        }

        let payload = if let Some(payload_len) = payload_len {
            if data.len() < header_len + payload_len {
                return Err(PcfError("pcf: truncated payload"));
            }
            &data[header_len..header_len + payload_len]
        } else {
            &data[header_len..]
        };

        Ok(PcfHeader {
            flags,
            codec_magic,
            stream_id,
            seq,
            send_time_us,
            presentation_time_us,
            ref_seq,
            client_id,
            quality_index,
            render_primitive,
            payload,
        })
    }

    pub fn inner_codec_magic(data: &'a [u8]) -> Result<[u8; 3], PcfError> {
        let header = Self::parse(data)?;
        if let Some(codec_magic) = header.codec_magic {
            return Ok(codec_magic);
        }
        let magic = header
            .payload
            .get(0..3)
            .ok_or(PcfError("pcf: payload too short"))?;
        Ok([magic[0], magic[1], magic[2]])
    }
}

fn read_exact<'a>(
    data: &'a [u8],
    offset: &mut usize,
    header_len: usize,
    len: usize,
) -> Result<&'a [u8], PcfError> {
    if *offset + len > header_len || data.len() < *offset + len {
        return Err(PcfError("pcf: truncated header"));
    }
    let bytes = &data[*offset..*offset + len];
    *offset += len;
    Ok(bytes)
}

fn read_u32(data: &[u8], offset: &mut usize, header_len: usize) -> Result<u32, PcfError> {
    let bytes = read_exact(data, offset, header_len, 4)?;
    Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
}

fn read_u64(data: &[u8], offset: &mut usize, header_len: usize) -> Result<u64, PcfError> {
    let bytes = read_exact(data, offset, header_len, 8)?;
    Ok(u64::from_le_bytes(bytes.try_into().unwrap()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_frame_has_seven_byte_overhead() {
        let payload = b"GSPpayload";
        let mut frame = Vec::new();
        PcfHeader::write_frame_to(&mut frame, &PcfFrameMeta::default(), payload).unwrap();

        assert_eq!(frame.len(), PCF_BASE_HEADER_LEN + payload.len());
        assert_eq!(frame[4] as usize, PCF_BASE_HEADER_LEN);

        let header = PcfHeader::parse(&frame).unwrap();
        assert!(header.flags.is_empty());
        assert_eq!(header.payload, payload);
        assert_eq!(PcfHeader::inner_codec_magic(&frame).unwrap(), *b"GSP");
    }

    #[test]
    fn optional_metadata_round_trips_without_payload_length() {
        let payload = b"GSPpayload";
        let meta = PcfFrameMeta {
            key: true,
            codec_magic: Some(*b"GSP"),
            stream_id: Some(42),
            seq: Some(7),
            send_time_us: Some(1000),
            presentation_time_us: Some(2000),
            client_id: Some(3),
            quality_index: Some(1),
            render_primitive: Some(RenderPrimitive::GaussianSplats),
            ..Default::default()
        };
        let mut frame = Vec::new();
        PcfHeader::write_frame_to(&mut frame, &meta, payload).unwrap();

        let header = PcfHeader::parse(&frame).unwrap();
        assert!(header.flags.contains(Flags::KEY));
        assert!(!header.flags.contains(Flags::HAS_PAYLOAD_LEN));
        assert_eq!(header.codec_magic, Some(*b"GSP"));
        assert_eq!(header.stream_id, Some(42));
        assert_eq!(header.seq, Some(7));
        assert_eq!(header.send_time_us, Some(1000));
        assert_eq!(header.presentation_time_us, Some(2000));
        assert_eq!(header.client_id, Some(3));
        assert_eq!(header.quality_index, Some(1));
        assert_eq!(
            header.render_primitive,
            Some(RenderPrimitive::GaussianSplats)
        );
        assert_eq!(header.payload, payload);
    }
}
