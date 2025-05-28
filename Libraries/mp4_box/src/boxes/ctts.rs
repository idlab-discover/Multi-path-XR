use crate::format_fourcc;
use super::generic::Mp4Box;

/// Represents a single entry in the `CttsBox`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CttsEntry {
    pub sample_count: u32,
    pub sample_offset: i32,  // Always stored as i32 for internal consistency
}

/// The `CttsBox` represents the Composition Time to Sample Box in MP4.
/// It maps samples to their composition time offsets.
#[derive(Clone)]
pub struct CttsBox {
    pub version: u8,
    pub flags: u32,
    pub entries: Vec<CttsEntry>,
}

impl Default for CttsBox {
    fn default() -> Self {
        CttsBox {
            version: 0,
            flags: 0,
            entries: Vec::new(),
        }
    }
}

impl std::fmt::Debug for CttsBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CttsBox")
            .field("box_size", &self.box_size())
            .field("box_type", &format_fourcc(&self.box_type()))
            .field("version", &self.version)
            .field("flags", &self.flags)
            .field("entries", &self.entries)
            .finish()
    }
}

impl Mp4Box for CttsBox {
    fn box_type(&self) -> [u8; 4] { *b"ctts" }

    fn box_size(&self) -> u32 {
        8 + 4 + 4 + (self.entries.len() as u32) * 8
        // 8 = header, 4 = version+flags, 4 = entry_count, each entry = 8 bytes
    }

    fn write_box(&self, buffer: &mut Vec<u8>) {
        buffer.extend_from_slice(&self.box_size().to_be_bytes());
        buffer.extend_from_slice(&self.box_type());
        buffer.push(self.version);
        buffer.extend_from_slice(&self.flags.to_be_bytes()[1..4]);  // Only 3 bytes for flags

        buffer.extend_from_slice(&(self.entries.len() as u32).to_be_bytes());

        for entry in &self.entries {
            buffer.extend_from_slice(&entry.sample_count.to_be_bytes());
            match self.version {
                0 => {
                    // Version 0: store sample_offset as u32 (even if internally i32)
                    buffer.extend_from_slice(&(entry.sample_offset as u32).to_be_bytes());
                }
                1 => {
                    buffer.extend_from_slice(&entry.sample_offset.to_be_bytes());
                }
                _ => panic!("Unsupported CTTS version during write"),
            }
        }
    }

    fn read_box(data: &[u8]) -> Result<(Self, usize), String> {
        if data.len() < 16 {
            return Err("CTTS box too small".into());
        }

        let size = u32::from_be_bytes(data[0..4].try_into().unwrap()) as usize;
        if data.len() < size {
            return Err("Incomplete CTTS box".into());
        }
        if &data[4..8] != b"ctts" {
            return Err("Not a CTTS box".into());
        }

        let version = data[8];
        if version > 1 {
            return Err(format!("Unsupported CTTS version: {}", version));
        }

        let flags = {
            let mut f = [0u8; 4];
            f[1..4].copy_from_slice(&data[9..12]);
            u32::from_be_bytes(f)
        };

        let entry_count = u32::from_be_bytes(data[12..16].try_into().unwrap());
        let expected_size = 16 + (entry_count as usize) * 8;
        if size < expected_size {
            return Err("CTTS box size mismatch with entry count".into());
        }

        let mut entries = Vec::with_capacity(entry_count as usize);
        let mut offset = 16;

        for _ in 0..entry_count {
            let sample_count = u32::from_be_bytes(data[offset..offset+4].try_into().unwrap());
            let sample_offset = match version {
                0 => u32::from_be_bytes(data[offset+4..offset+8].try_into().unwrap()) as i32,
                1 => i32::from_be_bytes(data[offset+4..offset+8].try_into().unwrap()),
                _ => unreachable!(),
            };
            entries.push(CttsEntry { sample_count, sample_offset });
            offset += 8;
        }

        Ok((
            CttsBox { version, flags, entries },
            size
        ))
    }
}
