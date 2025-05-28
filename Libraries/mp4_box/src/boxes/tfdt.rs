use crate::format_fourcc;

use super::generic::Mp4Box;

// The `TfdtBox` struct represents a Track Fragment Decode Time Box in the MP4 file format.
// This box specifies the decode time of the first sample in a track fragment.
// It is used in fragmented MP4 files to provide timing information for track fragments.
//
// Fields:
// - `base_decode_time`: A 64-bit unsigned integer representing the timeline position of the first sample in timescale units.
//   This value is expressed in the timescale of the movie and provides the decode time for the first sample in the fragment.
#[derive(Default, Clone)]
pub struct TfdtBox { // Track Fragment Decode Time Box
    pub version: u8,             // 0 = 32-bit, 1 = 64-bit
    pub flags: u32,              // 24-bit flags
    pub base_decode_time: u64,  // Timeline position in timescale units
}

impl std::fmt::Debug for TfdtBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TfdtBox")
            .field("box_size", &self.box_size())
            .field("box_type", &format_fourcc(&self.box_type()))
            .field("version", &self.version)
            .field("flags", &self.flags)
            .field("base_decode_time", &self.base_decode_time)
            .finish()
    }
}

// Implementation of the `Mp4Box` trait for the `TfdtBox` struct.
impl Mp4Box for TfdtBox {
    // Returns the box type as a 4-byte array. For `TfdtBox`, the type is "tfdt".
    fn box_type(&self) -> [u8; 4] { *b"tfdt" }

    // Calculates the size of the `TfdtBox` in bytes.
    // The size includes:
    // - 8 bytes for the header (4 bytes for size and 4 bytes for type).
    // - 4 bytes for the version and flags.
    // - 8 bytes for the `base_decode_time` field.
    fn box_size(&self) -> u32 {
        8 + 4 + if self.version == 1 { 8 } else { 4 }
        // 8 header + 4 version/flags + decode time (32 or 64 bits)
    }

    // Writes the `TfdtBox` to the provided buffer.
    // The method serializes the box size, box type, version, flags, and `base_decode_time` into the buffer.
    fn write_box(&self, buffer: &mut Vec<u8>) {
        buffer.extend_from_slice(&self.box_size().to_be_bytes());
        buffer.extend_from_slice(&self.box_type());
        buffer.push(self.version);
        buffer.extend_from_slice(&self.flags.to_be_bytes()[1..4]);  // flags (24-bit)

        if self.version == 1 {
            buffer.extend_from_slice(&self.base_decode_time.to_be_bytes());
        } else if self.version == 0 {
            buffer.extend_from_slice(&(self.base_decode_time as u32).to_be_bytes());
        } else {
            panic!("Unsupported TFDT version: {}", self.version);
        }
    }
    
    fn read_box(data: &[u8]) -> Result<(Self, usize), String> {
        let size = u32::from_be_bytes(data[0..4].try_into().unwrap()) as usize;
        if data.len() < size {
            return Err("Incomplete TFDT box".into());
        }
        if &data[4..8] != b"tfdt" {
            return Err("Not a TFDT box".into());
        }

        let version = data[8];
        let mut flag_bytes = [0u8; 4];
        flag_bytes[1..4].copy_from_slice(&data[9..12]);
        let flags = u32::from_be_bytes(flag_bytes);

        let base_decode_time = if version == 1 {
            u64::from_be_bytes(data[12..20].try_into().unwrap())
        } else if version == 0 {
            u32::from_be_bytes(data[12..16].try_into().unwrap()) as u64
        } else {
            return Err(format!("Unsupported TFDT version: {}", version));
        };

        Ok((
            TfdtBox { version, flags, base_decode_time },
            size
        ))
    }
}
