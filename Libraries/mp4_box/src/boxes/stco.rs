use crate::format_fourcc;

use super::generic::Mp4Box;

// The `StcoBox` struct represents a Chunk Offset Box in the MP4 file format.
// This box contains a table of chunk offsets, which specify the location of each chunk in the media data.
// The offsets are file offsets, indicating the position of the chunk relative to the beginning of the file.
//
// Fields:
// - `version`: Full box version (always 0 per spec, but configurable).
// - `flags`: Full box flags (typically 0).
// - `entries`: A list of 32-bit chunk offsets.
//
// The `StcoBox` is essential for enabling efficient access to media data chunks, as it provides the mapping
// between chunk indices and their corresponding file offsets.
#[derive(Clone)]
pub struct StcoBox {  // Chunk Offset Box
    pub version: u8,        // Full box version (should be 0)
    pub flags: u32,         // Full box flags (24 bits used)
    pub entries: Vec<u32>,  // List of chunk offsets
}

// Provides a default implementation for the `StcoBox` struct.
// The default `StcoBox` represents an empty box with no entries.
impl Default for StcoBox {
    fn default() -> Self {
        StcoBox {
            version: 0,
            flags: 0,
            entries: vec![
                0,
            ],
        }
    }
}

impl std::fmt::Debug for StcoBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StcoBox")
            .field("box_size", &self.box_size())
            .field("box_type", &format_fourcc(&self.box_type()))
            .field("version", &self.version)
            .field("flags", &self.flags)
            .field("entries", &self.entries)
            .finish()
    }
}

// Implementation of the `Mp4Box` trait for the `StcoBox` struct.
impl Mp4Box for StcoBox {
    // Returns the box type as a 4-byte array. For `StcoBox`, the type is "stco".
    fn box_type(&self) -> [u8; 4] { *b"stco" }

    // Calculates the size of the `StcoBox` in bytes.
    // The size is fixed at 16 bytes, which includes:
    // - 8 bytes for the header (4 bytes for size and 4 bytes for type).
    // - 4 bytes for the version and flags.
    // - 4 bytes for the `entry_count` field.
    // - 4 bytes for each entry in the `entries` vector.
    fn box_size(&self) -> u32 { 
        8 + 4 + 4 + (4 * self.entries.len() as u32)
     }

    // Writes the `StcoBox` to the provided buffer.
    // The method serializes the box size, box type, version, flags, and `entry_count` into the buffer.
    fn write_box(&self, buffer: &mut Vec<u8>) {
        // Write the size of the box in big-endian format.
        buffer.extend_from_slice(&self.box_size().to_be_bytes());
        // Write the box type ("stco").
        buffer.extend_from_slice(&self.box_type());
        // Write the version (1 byte) and flags (3 bytes).
        buffer.push(self.version);
        buffer.extend_from_slice(&self.flags.to_be_bytes()[1..4]);
        // Write the `entry_count` field, which is set to 0 in this implementation.
        buffer.extend_from_slice(&0u32.to_be_bytes());
    }

    fn read_box(data: &[u8]) -> Result<(Self, usize), String> {
        let size = u32::from_be_bytes(data[0..4].try_into().unwrap()) as usize;
        if data.len() < size {
            return Err("Incomplete STCO box".into());
        }
        if &data[4..8] != b"stco" {
            return Err("Not an STCO box".into());
        }

        let version = data[8];
        let flags = {
            let mut f = [0u8; 4];
            f[1..4].copy_from_slice(&data[9..12]);
            u32::from_be_bytes(f)
        };

        let entry_count = u32::from_be_bytes(data[12..16].try_into().unwrap());
        if size < 16 + (entry_count as usize) * 4 {
            return Err("STCO box size mismatch with entry count".into());
        }

        let mut entries = Vec::with_capacity(entry_count as usize);
        let mut offset = 16;
        for _ in 0..entry_count {
            let chunk_offset = u32::from_be_bytes(data[offset..offset + 4].try_into().unwrap());
            entries.push(chunk_offset);
            offset += 4;
        }

        Ok((
            StcoBox {
                version,
                flags,
                entries,
            },
            size,
        ))
    }
}
