use crate::format_fourcc;

use super::generic::Mp4Box;

/// The `ElstBox` struct represents an Edit List Box (`elst`) in the MP4 file format.
/// It defines the mapping from media time to presentation time, allowing complex edits.
///
/// Fields:
/// - `version`: Indicates the version of the box (0 or 1).
/// - `flags`: 24-bit flags (typically unused).
/// - `entries`: List of edit entries, each specifying a segment duration, media time, and playback rate.
#[derive(Default, Clone)]
pub struct ElstBox { // Edit List Box
    pub version: u8,
    pub flags: u32,
    pub entries: Vec<ElstEntry>, // List of edit entries
}

/// The `ElstEntry` struct represents a single edit list entry.
/// Each entry maps a segment of the media to a time offset and playback rate.
#[derive(Default, Clone)]
pub struct ElstEntry {
    pub segment_duration: u64,
    pub media_time: u64,
    pub media_rate: u16,
    pub media_rate_fraction: u16,
}

impl std::fmt::Debug for ElstBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ElstBox")
            .field("box_size", &self.box_size())
            .field("box_type", &format_fourcc(&self.box_type()))
            .field("version", &self.version)
            .field("flags", &format!("0x{:06X}", self.flags))
            .field("entries", &self.entries)
            .finish()
    }
}

impl std::fmt::Debug for ElstEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ElstEntry")
            .field("segment_duration", &self.segment_duration)
            .field("media_time", &self.media_time)
            .field("media_rate", &self.media_rate)
            .field("media_rate_fraction", &self.media_rate_fraction)
            .finish()
    }
}

// Implementation of the `Mp4Box` trait for the `ElstBox` struct.
impl Mp4Box for ElstBox {
    fn box_type(&self) -> [u8; 4] { *b"elst" }

    fn box_size(&self) -> u32 {
        let entry_size = if self.version == 1 { 20 } else { 12 };
        8 + 4 + (self.entries.len() as u32 * entry_size)
    }

    fn write_box(&self, buffer: &mut Vec<u8>) {
        buffer.extend_from_slice(&self.box_size().to_be_bytes());
        buffer.extend_from_slice(&self.box_type());

        buffer.push(self.version);
        buffer.extend_from_slice(&(self.flags & 0x00FFFFFF).to_be_bytes()[1..]);

        buffer.extend_from_slice(&(self.entries.len() as u32).to_be_bytes());

        for entry in &self.entries {
            if self.version == 1 {
                buffer.extend_from_slice(&entry.segment_duration.to_be_bytes());
                buffer.extend_from_slice(&entry.media_time.to_be_bytes());
            } else {
                buffer.extend_from_slice(&(entry.segment_duration as u32).to_be_bytes());
                buffer.extend_from_slice(&(entry.media_time as u32).to_be_bytes());
            }
            buffer.extend_from_slice(&entry.media_rate.to_be_bytes());
            buffer.extend_from_slice(&entry.media_rate_fraction.to_be_bytes());
        }
    }

    fn read_box(data: &[u8]) -> Result<(Self, usize), String> {
        let size = u32::from_be_bytes(data[0..4].try_into().unwrap()) as usize;
        if data.len() < size {
            return Err("Incomplete ELST box".into());
        }
        if &data[4..8] != b"elst" {
            return Err("Not an ELST box".into());
        }

        let version = data[8];
        let flags = u32::from_be_bytes([0, data[9], data[10], data[11]]);
        let entry_count = u32::from_be_bytes(data[12..16].try_into().unwrap());

        let mut offset = 16;
        let mut entries = Vec::with_capacity(entry_count as usize);

        for _ in 0..entry_count {
            let (segment_duration, media_time) = if version == 1 {
                (
                    u64::from_be_bytes(data[offset..offset+8].try_into().unwrap()),
                    u64::from_be_bytes(data[offset+8..offset+16].try_into().unwrap()),
                )
            } else {
                (
                    u32::from_be_bytes(data[offset..offset+4].try_into().unwrap()) as u64,
                    u32::from_be_bytes(data[offset+4..offset+8].try_into().unwrap()) as u64,
                )
            };
            let rate_offset = if version == 1 { offset + 16 } else { offset + 8 };
            let media_rate = u16::from_be_bytes(data[rate_offset..rate_offset+2].try_into().unwrap());
            let media_rate_fraction = u16::from_be_bytes(data[rate_offset+2..rate_offset+4].try_into().unwrap());

            entries.push(ElstEntry {
                segment_duration,
                media_time,
                media_rate,
                media_rate_fraction,
            });

            offset += if version == 1 { 20 } else { 12 };
        }

        Ok((
            ElstBox { version, flags, entries },
            size
        ))
    }
}
