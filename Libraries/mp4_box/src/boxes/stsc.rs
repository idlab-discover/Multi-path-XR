use crate::format_fourcc;

use super::generic::Mp4Box;

// The `StscBox` struct represents a Sample-to-Chunk Box in the MP4 file format.
// This box contains a table that maps samples to chunks, specifying how samples are grouped into chunks.
// The table provides information about the first chunk, the number of samples per chunk, and the sample description index.
//
// Fields:
// - `version`: Full box version (usually 0).
// - `flags`: Full box flags (24 bits, usually 0).
// - This implementation of `StscBox` does not currently include any fields, as it represents an empty box with no entries.
//
// The `StscBox` is essential for enabling efficient access to media samples, as it provides the mapping
// between sample indices and their corresponding chunks.
#[derive(Clone)]
pub struct StscBox { // Sample-to-Chunk Box
    pub version: u8,
    pub flags: u32,
    pub entries: Vec<StscEntry>,
}

#[derive(Clone)]
pub struct StscEntry {
    pub first_chunk: u32,
    pub samples_per_chunk: u32,
    pub sample_description_index: u32,
    pub first_sample: u32,
}
// Provides a default implementation for the `StscBox` struct.
impl Default for StscBox {
    fn default() -> Self {
        StscBox {
            version: 0,
            flags: 0,
            entries: vec![StscEntry::default()],
        }
    }
}

impl Default for StscEntry {
    fn default() -> Self {
        StscEntry {
            first_chunk: 1,
            samples_per_chunk: 1,
            sample_description_index: 1,
            first_sample: 1,
        }
    }
}

impl std::fmt::Debug for StscBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StscBox")
            .field("box_size", &self.box_size())
            .field("box_type", &format_fourcc(&self.box_type()))
            .field("version", &self.version)
            .field("flags", &self.flags)
            .field("entries", &self.entries)
            .finish()
    }
}

impl std::fmt::Debug for StscEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StscEntry")
            .field("first_chunk", &self.first_chunk)
            .field("samples_per_chunk", &self.samples_per_chunk)
            .field("sample_description_index", &self.sample_description_index)
            .field("first_sample", &self.first_sample)
            .finish()
    }
}

// Implementation of the `Mp4Box` trait for the `StscBox` struct.
impl Mp4Box for StscBox {
    // Returns the box type as a 4-byte array. For `StscBox`, the type is "stsc".
    fn box_type(&self) -> [u8; 4] { *b"stsc" }

    // Calculates the size of the `StscBox` in bytes.
    // The size is fixed at 16 bytes, which includes:
    // - 8 bytes for the header (4 bytes for size and 4 bytes for type).
    // - 4 bytes for the version and flags.
    // - 4 bytes for the `entry_count` field.
    // - 4 bytes for each entry in the `entries` vector.
    fn box_size(&self) -> u32 { 
        8 + 4 + 4 + (12 * self.entries.len() as u32)
     }

    // Writes the `StscBox` to the provided buffer.
    // The method serializes the box size, box type, version, flags, and `entry_count` into the buffer.
    fn write_box(&self, buffer: &mut Vec<u8>) {
        // Write the size of the box in big-endian format.
        buffer.extend_from_slice(&self.box_size().to_be_bytes());
        // Write the box type ("stsc").
        buffer.extend_from_slice(&self.box_type());
        // Write the version (1 byte) and flags (3 bytes).
        buffer.push(self.version);
        buffer.extend_from_slice(&self.flags.to_be_bytes()[1..4]);  // flags (24 bits)

        buffer.extend_from_slice(&(self.entries.len() as u32).to_be_bytes());

        for entry in &self.entries {
            buffer.extend_from_slice(&entry.first_chunk.to_be_bytes());
            buffer.extend_from_slice(&entry.samples_per_chunk.to_be_bytes());
            buffer.extend_from_slice(&entry.sample_description_index.to_be_bytes());
        }
    }

    fn read_box(data: &[u8]) -> Result<(Self, usize), String> {
        let size = u32::from_be_bytes(data[0..4].try_into().unwrap()) as usize;
        if data.len() < size {
            return Err("Incomplete STSC box".into());
        }
        if &data[4..8] != b"stsc" {
            return Err("Not an STSC box".into());
        }

        let version = data[8];
        let flags = {
            let mut f = [0u8; 4];
            f[1..4].copy_from_slice(&data[9..12]);
            u32::from_be_bytes(f)
        };

        let entry_count = u32::from_be_bytes(data[12..16].try_into().unwrap());
        if size < 16 + (entry_count as usize) * 12 {
            return Err("STSC box size mismatch with entry count".into());
        }

        let mut entries = Vec::with_capacity(entry_count as usize);
        let mut offset = 16;
        for _ in 0..entry_count {
            let first_chunk = u32::from_be_bytes(data[offset..offset+4].try_into().unwrap());
            let samples_per_chunk = u32::from_be_bytes(data[offset+4..offset+8].try_into().unwrap());
            let sample_description_index = u32::from_be_bytes(data[offset+8..offset+12].try_into().unwrap());
            entries.push(StscEntry {
                first_chunk,
                samples_per_chunk,
                sample_description_index,
                first_sample: 0,  // Will be computed if needed
            });
            offset += 12;
        }

        Ok((StscBox { version, flags, entries }, size))
    }
}
