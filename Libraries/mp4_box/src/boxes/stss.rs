use crate::format_fourcc;
use super::generic::Mp4Box;

/// The `StssBox` (Sync Sample Box) lists the samples that are sync points (keyframes).
/// If this box is not present, all samples are considered sync samples.
#[derive(Clone)]
pub struct StssBox {
    pub version: u8,         // Full box version
    pub flags: u32,          // Full box flags (24 bits used)
    pub entries: Vec<u32>,   // List of sample numbers (1-based index)
}

impl Default for StssBox {
    fn default() -> Self {
        StssBox {
            version: 0,
            flags: 0,
            entries: Vec::new(),
        }
    }
}

impl std::fmt::Debug for StssBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StssBox")
            .field("box_size", &self.box_size())
            .field("box_type", &format_fourcc(&self.box_type()))
            .field("version", &self.version)
            .field("flags", &self.flags)
            .field("entries", &self.entries)
            .finish()
    }
}

impl Mp4Box for StssBox {
    fn box_type(&self) -> [u8; 4] { *b"stss" }

    fn box_size(&self) -> u32 {
        8 + 4 + 4 + (self.entries.len() as u32) * 4
        // 8 = header, 4 = version+flags, 4 = entry_count, each entry = 4 bytes
    }

    fn write_box(&self, buffer: &mut Vec<u8>) {
        buffer.extend_from_slice(&self.box_size().to_be_bytes());
        buffer.extend_from_slice(&self.box_type());

        // Write version and flags
        buffer.push(self.version);
        buffer.extend_from_slice(&self.flags.to_be_bytes()[1..4]);  // Only 3 bytes for flags

        // Write number of entries
        buffer.extend_from_slice(&(self.entries.len() as u32).to_be_bytes());

        // Write each sync sample number
        for sample_number in &self.entries {
            buffer.extend_from_slice(&sample_number.to_be_bytes());
        }
    }

    fn read_box(data: &[u8]) -> Result<(Self, usize), String> {
        if data.len() < 16 {
            return Err("STSS box too small".into());
        }

        let size = u32::from_be_bytes(data[0..4].try_into().unwrap()) as usize;
        if data.len() < size {
            return Err("Incomplete STSS box".into());
        }
        if &data[4..8] != b"stss" {
            return Err("Not an STSS box".into());
        }

        let version = data[8];
        let flags = {
            let mut f = [0u8; 4];
            f[1..4].copy_from_slice(&data[9..12]);
            u32::from_be_bytes(f)
        };

        let entry_count = u32::from_be_bytes(data[12..16].try_into().unwrap());
        if size < 16 + (entry_count as usize) * 4 {
            return Err("STSS box size mismatch with entry count".into());
        }

        let mut entries = Vec::with_capacity(entry_count as usize);
        let mut offset = 16;
        for _ in 0..entry_count {
            let sample_number = u32::from_be_bytes(data[offset..offset+4].try_into().unwrap());
            entries.push(sample_number);
            offset += 4;
        }

        Ok((
            StssBox { version, flags, entries },
            size
        ))
    }
}
