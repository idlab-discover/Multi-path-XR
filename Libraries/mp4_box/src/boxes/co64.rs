use crate::format_fourcc;
use super::generic::Mp4Box;

/// The `Co64Box` (Chunk Offset Box - 64-bit) provides the file offsets for each chunk.
/// This box is used when 32-bit offsets (stco) are insufficient.
///
/// Fields:
/// - `version`: Full box version (always 0 per spec, but kept configurable).
/// - `flags`: Full box flags (24 bits used, typically 0).
/// - `entries`: A list of 64-bit chunk offsets.
#[derive(Clone)]
pub struct Co64Box {
    pub version: u8,        // Full box version (should be 0)
    pub flags: u32,         // Full box flags (24 bits)
    pub entries: Vec<u64>,  // 64-bit chunk offsets
}

impl Default for Co64Box {
    fn default() -> Self {
        Co64Box {
            version: 0,
            flags: 0,
            entries: Vec::new(),
        }
    }
}

impl std::fmt::Debug for Co64Box {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Co64Box")
            .field("box_size", &self.box_size())
            .field("box_type", &format_fourcc(&self.box_type()))
            .field("version", &self.version)
            .field("flags", &self.flags)
            .field("entries", &self.entries)
            .finish()
    }
}

impl Mp4Box for Co64Box {
    fn box_type(&self) -> [u8; 4] { *b"co64" }

    fn box_size(&self) -> u32 {
        8 + 4 + 4 + (self.entries.len() as u32) * 8
        // 8 = header, 4 = version+flags, 4 = entry_count, each entry = 8 bytes
    }

    fn write_box(&self, buffer: &mut Vec<u8>) {
        buffer.extend_from_slice(&self.box_size().to_be_bytes());
        buffer.extend_from_slice(&self.box_type());

        // Write version and flags (flags is 24 bits)
        buffer.push(self.version);
        buffer.extend_from_slice(&self.flags.to_be_bytes()[1..4]);  // Only last 3 bytes

        // Write number of entries
        buffer.extend_from_slice(&(self.entries.len() as u32).to_be_bytes());

        // Write each 64-bit chunk offset
        for offset in &self.entries {
            buffer.extend_from_slice(&offset.to_be_bytes());
        }
    }

    fn read_box(data: &[u8]) -> Result<(Self, usize), String> {
        if data.len() < 16 {
            return Err("CO64 box too small".into());
        }

        let size = u32::from_be_bytes(data[0..4].try_into().unwrap()) as usize;
        if data.len() < size {
            return Err("Incomplete CO64 box".into());
        }
        if &data[4..8] != b"co64" {
            return Err("Not a CO64 box".into());
        }

        let version = data[8];
        let flags = {
            let mut f = [0u8; 4];
            f[1..4].copy_from_slice(&data[9..12]);
            u32::from_be_bytes(f)
        };

        let entry_count = u32::from_be_bytes(data[12..16].try_into().unwrap());
        if size < 16 + (entry_count as usize) * 8 {
            return Err("CO64 box size mismatch with entry count".into());
        }

        let mut entries = Vec::with_capacity(entry_count as usize);
        let mut offset = 16;
        for _ in 0..entry_count {
            let chunk_offset = u64::from_be_bytes(data[offset..offset+8].try_into().unwrap());
            entries.push(chunk_offset);
            offset += 8;
        }

        Ok((
            Co64Box { version, flags, entries },
            size
        ))
    }
}
