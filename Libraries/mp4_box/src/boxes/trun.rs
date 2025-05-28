use crate::format_fourcc;

use super::generic::Mp4Box;

/// The `TrunBox` struct represents a Track Fragment Run Box (`trun`) in the MP4 file format.
/// It specifies sample information inside a track fragment, such as offsets and sample sizes.
///
/// Fields:
/// - `version`: Version of the box.
/// - `flags`: Flags indicating presence of fields (we only use data_offset and sample_size).
/// - `data_offset`: Offset of first sample relative to `mdat`.
/// - `sample_size`: Size of the single sample in bytes.
#[derive(Clone)]
pub struct TrunBox { // Track Fragment Run Box
    pub version: u8,
    pub flags: u32,
    pub data_offset: i32,
    pub sample_size: u32,
}

impl Default for TrunBox {
    fn default() -> Self {
        TrunBox {
            version: 0,
            flags: 0x000005, // data-offset-present + sample-size-present
            data_offset: 0,
            sample_size: 0,
        }
    }
}

impl std::fmt::Debug for TrunBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TrunBox")
            .field("box_size", &self.box_size())
            .field("box_type", &format_fourcc(&self.box_type()))
            .field("version", &self.version)
            .field("flags", &format!("0x{:06X}", self.flags))
            .field("data_offset", &self.data_offset)
            .field("sample_size", &self.sample_size)
            .finish()
    }
}

impl Mp4Box for TrunBox {
    fn box_type(&self) -> [u8; 4] { *b"trun" }

    fn box_size(&self) -> u32 {
        8 + 4 + 4 + 4 + 4  // header + version/flags + sample_count + data_offset + sample_size
    }

    fn write_box(&self, buffer: &mut Vec<u8>) {
        buffer.extend_from_slice(&self.box_size().to_be_bytes());
        buffer.extend_from_slice(&self.box_type());
        buffer.push(self.version);
        buffer.extend_from_slice(&(self.flags & 0x00FFFFFF).to_be_bytes()[1..]);
        buffer.extend_from_slice(&1u32.to_be_bytes()); // sample_count = 1
        buffer.extend_from_slice(&(self.data_offset as i32).to_be_bytes());
        buffer.extend_from_slice(&self.sample_size.to_be_bytes());
    }

    fn read_box(data: &[u8]) -> Result<(Self, usize), String> {
        let size = u32::from_be_bytes(data[0..4].try_into().unwrap()) as usize;
        if data.len() < size {
            return Err("Incomplete TRUN box".into());
        }
        if &data[4..8] != b"trun" {
            return Err("Not a TRUN box".into());
        }

        let version = data[8];
        let flags = u32::from_be_bytes([0, data[9], data[10], data[11]]);
        if flags != 0x000005 {
            return Err(format!("Unsupported TRUN flags: 0x{:06X}", flags));
        }

        let sample_count = u32::from_be_bytes(data[12..16].try_into().unwrap());
        if sample_count != 1 {
            return Err(format!("Expected 1 sample, got {}", sample_count));
        }

        let data_offset = i32::from_be_bytes(data[16..20].try_into().unwrap());
        let sample_size = u32::from_be_bytes(data[20..24].try_into().unwrap());

        Ok((
            TrunBox {
                version,
                flags,
                data_offset,
                sample_size,
            },
            size
        ))
    }
}
