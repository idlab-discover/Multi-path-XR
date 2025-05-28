use crate::format_fourcc;

use super::generic::Mp4Box;

// The `SttsBox` struct represents a Time-to-Sample Box in the MP4 file format.
// This box contains a table that maps decoding times to samples, specifying the duration of each sample.
// The table is used to determine the timing of media samples during playback.
//
// Fields:
// - `version`: The version of the box.
// - `flags`: Flags indicating specific properties of the box.
// - `entries`: A vector of `SttsEntry` instances, where each entry specifies the number of samples
//
// The `SttsBox` is essential for enabling accurate playback timing, as it provides the mapping
// between sample indices and their corresponding decoding times.
#[derive(Clone)]
pub struct SttsBox { // Time to Sample Box
    pub version: u8,
    pub flags: u32,
    pub entries: Vec<SttsEntry>, // List of time-to-sample entries
}

#[derive(Default, Clone)]
pub struct SttsEntry {
    pub sample_count: u32,
    pub sample_delta: u32,
}


// Provides a default implementation for the `SttsBox` struct.
// The default `SttsBox` represents an empty box with no entries.
impl Default for SttsBox {
    fn default() -> Self {
        SttsBox {
            version: 0,
            flags: 0,
            entries: vec![
                SttsEntry::default()
            ],
        }
    }
}

impl std::fmt::Debug for SttsBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SttsBox")
            .field("box_size", &self.box_size())
            .field("box_type", &format_fourcc(&self.box_type()))
            .field("version", &self.version)
            .field("flags", &self.flags)
            .field("entries", &self.entries)
            .finish()
    }
}


impl std::fmt::Debug for SttsEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SttsEntry")
            .field("sample_count", &self.sample_count)
            .field("sample_delta", &self.sample_delta)
            .finish()
    }
}

// Implementation of the `Mp4Box` trait for the `SttsBox` struct.
impl Mp4Box for SttsBox {
    // Returns the box type as a 4-byte array. For `SttsBox`, the type is "stts".
    fn box_type(&self) -> [u8; 4] { *b"stts" }

    // Calculates the size of the `SttsBox` in bytes.
    // The size is fixed at 16 bytes, which includes:
    // - 8 bytes for the header (4 bytes for size and 4 bytes for type).
    // - 4 bytes for the version and flags.
    // - 4 bytes for the `entry_count` field, which is set to 0 in this implementation.
    fn box_size(&self) -> u32 {
        8 + 4 + 4 + (self.entries.len() as u32 * 8)
        // 8 header + 4 version/flags + 4 entry_count + entries
    }

    // Writes the `SttsBox` to the provided buffer.
    // The method serializes the box size, box type, version, flags, and `entry_count` into the buffer.
    fn write_box(&self, buffer: &mut Vec<u8>) {
        buffer.extend_from_slice(&self.box_size().to_be_bytes());
        buffer.extend_from_slice(&self.box_type());
        buffer.push(0);  // version
        buffer.extend_from_slice(&[0; 3]);  // flags
        buffer.extend_from_slice(&0u32.to_be_bytes());  // entry_count = 0

        for entry in &self.entries {
            buffer.extend_from_slice(&entry.sample_count.to_be_bytes());
            buffer.extend_from_slice(&entry.sample_delta.to_be_bytes());
        }
    }

    fn read_box(data: &[u8]) -> Result<(Self, usize), String> {
        let size = u32::from_be_bytes(data[0..4].try_into().unwrap()) as usize;
        if data.len() < size {
            return Err("Incomplete STTS box".into());
        }
        if &data[4..8] != b"stts" {
            return Err("Not an STTS box".into());
        }

        let version = data[8];
        let mut flag_bytes = [0u8; 4];
        flag_bytes[1..4].copy_from_slice(&data[9..12]);
        let flags = u32::from_be_bytes(flag_bytes);

        let entry_count = u32::from_be_bytes(data[12..16].try_into().unwrap());
        let mut entries = Vec::new();
        let mut offset = 16;

        for _ in 0..entry_count {
            if offset + 8 > size { return Err("Incomplete STTS entry".into()); }
            let sample_count = u32::from_be_bytes(data[offset..offset+4].try_into().unwrap());
            let sample_delta = u32::from_be_bytes(data[offset+4..offset+8].try_into().unwrap());
            entries.push(SttsEntry { sample_count, sample_delta });
            offset += 8;
        }

        Ok((SttsBox { version, flags, entries }, size))
    }
}
