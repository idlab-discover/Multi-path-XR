use crate::types::*;
use std::collections::BTreeMap;

pub const PKT_MAGIC: &[u8; 3] = b"PKT";
pub const PKT_VERSION: u8 = 1;

pub const PKT_HEADER_LEN: usize = 3 + 1 + 4 + 8 + 2 + 2;

// Packet header: "PKT"(3) | v(1) | stream_id(4) | seq(8) | idx(2) | total(2) | data
pub fn split_into_chunks(
    stream_id: StreamId,
    seq: SeqNo,
    mtu: usize,
    frame: &[u8],
    out: &mut Vec<Vec<u8>>,
) {
    assert!(mtu > PKT_HEADER_LEN, "mtu too small");
    let max_payload = mtu - PKT_HEADER_LEN;
    let total = frame.len().div_ceil(max_payload) as u16;
    for (idx, chunk) in frame.chunks(max_payload).enumerate() {
        let mut v = Vec::with_capacity(mtu);
        v.extend_from_slice(PKT_MAGIC);
        v.push(PKT_VERSION);
        v.extend_from_slice(&le_u32(stream_id));
        v.extend_from_slice(&le_u64(seq));
        v.extend_from_slice(&(idx as u16).to_le_bytes());
        v.extend_from_slice(&total.to_le_bytes());
        v.extend_from_slice(chunk);
        out.push(v);
    }
}

#[derive(Default)]
pub struct Reassembler {
    // Keyed by (stream_id, seq)
    map: BTreeMap<(StreamId, SeqNo), Partial>,
}

struct Partial {
    total: u16,
    got: u16,
    parts: Vec<Option<Vec<u8>>>,
}

impl Reassembler {
    pub fn new() -> Self {
        Self {
            map: BTreeMap::new(),
        }
    }

    /// Returns Some(full_frame) when complete, else None.
    pub fn push_chunk(
        &mut self,
        data: &[u8],
    ) -> Result<Option<(StreamId, SeqNo, Vec<u8>)>, PcfError> {
        if data.len() < PKT_HEADER_LEN {
            return Err(PcfError("pkt: truncated"));
        }
        if &data[0..3] != PKT_MAGIC {
            return Err(PcfError("pkt: bad magic"));
        }
        if data[3] != PKT_VERSION {
            return Err(PcfError("pkt: bad version"));
        }
        let mut o = 4;
        let sid = u32::from_le_bytes(data[o..o + 4].try_into().unwrap());
        o += 4;
        let seq = u64::from_le_bytes(data[o..o + 8].try_into().unwrap());
        o += 8;
        let idx = u16::from_le_bytes(data[o..o + 2].try_into().unwrap()) as usize;
        o += 2;
        let total = u16::from_le_bytes(data[o..o + 2].try_into().unwrap());
        o += 2;
        let payload = &data[o..];

        let key = (sid, seq);
        let entry = self.map.entry(key).or_insert_with(|| Partial {
            total,
            got: 0,
            parts: vec![None; total as usize],
        });

        if entry.parts[idx].is_none() {
            entry.parts[idx] = Some(payload.to_vec());
            entry.got += 1;
        }
        if entry.got == entry.total {
            let mut frame = Vec::new();
            for p in entry.parts.iter_mut() {
                frame.extend_from_slice(p.as_ref().unwrap());
            }
            self.map.remove(&key);
            return Ok(Some((sid, seq, frame)));
        }
        Ok(None)
    }
}
