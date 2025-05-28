use crate::format_fourcc;

use super::generic::Mp4Box;

/// The `TrexBox` struct represents a Track Extends Box (`trex`) in the MP4 file format.
/// It defines default properties for track fragments used in fragmented MP4 files.
///
/// Fields:
/// - `version`: Version of the box.
/// - `flags`: Flags for the box (24 bits).
/// - `track_id`: ID of the track.
/// - `default_sample_description_index`: Default sample description index.
/// - `default_sample_duration`: Default duration for each sample.
/// - `default_sample_size`: Default size for each sample.
/// - `default_sample_flags`: Default sample flags for each sample.
#[derive(Clone)]
pub struct TrexBox { // Track Extends Box
    pub version: u8,
    pub flags: u32,
    pub track_id: u32,
    pub default_sample_description_index: u32,
    pub default_sample_duration: u32,
    pub default_sample_size: u32,
    pub default_sample_flags: u32,
}

impl Default for TrexBox {
    fn default() -> Self {
        TrexBox {
            version: 0,
            flags: 0,
            track_id: 1,
            default_sample_description_index: 1,
            default_sample_duration: 1000,
            default_sample_size: 0,
            default_sample_flags: 0x02000000,
        }
    }
}

impl std::fmt::Debug for TrexBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TrexBox")
            .field("box_size", &self.box_size())
            .field("box_type", &format_fourcc(&self.box_type()))
            .field("version", &self.version)
            .field("flags", &format!("0x{:06X}", self.flags))
            .field("track_id", &self.track_id)
            .field("default_sample_description_index", &self.default_sample_description_index)
            .field("default_sample_duration", &self.default_sample_duration)
            .field("default_sample_size", &self.default_sample_size)
            .field("default_sample_flags", &format!("0x{:08X}", self.default_sample_flags))
            .finish()
    }
}

impl Mp4Box for TrexBox {
    fn box_type(&self) -> [u8; 4] { *b"trex" }

    fn box_size(&self) -> u32 {
        8 + 4 + 5 * 4 // header + version/flags + 5 fields
    }

    fn write_box(&self, buffer: &mut Vec<u8>) {
        buffer.extend_from_slice(&self.box_size().to_be_bytes());
        buffer.extend_from_slice(&self.box_type());
        buffer.push(self.version);
        buffer.extend_from_slice(&(self.flags & 0x00FFFFFF).to_be_bytes()[1..]);
        buffer.extend_from_slice(&self.track_id.to_be_bytes());
        buffer.extend_from_slice(&self.default_sample_description_index.to_be_bytes());
        buffer.extend_from_slice(&self.default_sample_duration.to_be_bytes());
        buffer.extend_from_slice(&self.default_sample_size.to_be_bytes());
        buffer.extend_from_slice(&self.default_sample_flags.to_be_bytes());
    }

    fn read_box(data: &[u8]) -> Result<(Self, usize), String> {
        let size = u32::from_be_bytes(data[0..4].try_into().unwrap()) as usize;
        if data.len() < size {
            return Err("Incomplete TREX box".into());
        }
        if &data[4..8] != b"trex" {
            return Err("Not a TREX box".into());
        }

        let version = data[8];
        let flags = u32::from_be_bytes([0, data[9], data[10], data[11]]);
        let track_id = u32::from_be_bytes(data[12..16].try_into().unwrap());
        let default_sample_description_index = u32::from_be_bytes(data[16..20].try_into().unwrap());
        let default_sample_duration = u32::from_be_bytes(data[20..24].try_into().unwrap());
        let default_sample_size = u32::from_be_bytes(data[24..28].try_into().unwrap());
        let default_sample_flags = u32::from_be_bytes(data[28..32].try_into().unwrap());

        Ok((
            TrexBox {
                version,
                flags,
                track_id,
                default_sample_description_index,
                default_sample_duration,
                default_sample_size,
                default_sample_flags,
            },
            size
        ))
    }
}
