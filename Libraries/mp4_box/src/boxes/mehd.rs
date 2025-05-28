use crate::format_fourcc;
use super::generic::Mp4Box;

/// The `MehdBox` represents the Movie Extends Header Box in fragmented MP4 files.
/// It specifies the overall duration of the movie fragment.
/// 
/// - `version`: Determines if `fragment_duration` is stored as 32-bit (version 0) or 64-bit (version 1).
/// - `fragment_duration`: Duration of the entire presentation (in timescale units).
#[derive(Default, Clone)]
pub struct MehdBox {
    pub version: u8,               // 0 or 1
    pub fragment_duration: u64,    // Duration in timescale units
}


impl std::fmt::Debug for MehdBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MehdBox")
            .field("box_size", &self.box_size())
            .field("box_type", &format_fourcc(&self.box_type()))
            .field("version", &self.version)
            .field("fragment_duration", &self.fragment_duration)
            .finish()
    }
}

impl Mp4Box for MehdBox {
    fn box_type(&self) -> [u8; 4] { *b"mehd" }

    fn box_size(&self) -> u32 {
        8 + 4 + if self.version == 1 { 8 } else { 4 }
        // 8 bytes header + 4 bytes version/flags + duration field
    }

    fn write_box(&self, buffer: &mut Vec<u8>) {
        buffer.extend_from_slice(&self.box_size().to_be_bytes());
        buffer.extend_from_slice(&self.box_type());

        // Write version + flags (flags are always zero here)
        buffer.push(self.version);
        buffer.extend_from_slice(&[0; 3]);

        // Write duration depending on version
        if self.version == 1 {
            buffer.extend_from_slice(&self.fragment_duration.to_be_bytes());
        } else {
            buffer.extend_from_slice(&(self.fragment_duration as u32).to_be_bytes());
        }
    }

    fn read_box(data: &[u8]) -> Result<(Self, usize), String> {
        if data.len() < 16 {
            return Err("MEHD box too small".into());
        }

        let size = u32::from_be_bytes(data[0..4].try_into().unwrap()) as usize;
        if data.len() < size {
            return Err("Incomplete MEHD box".into());
        }

        if &data[4..8] != b"mehd" {
            return Err("Not a MEHD box".into());
        }

        let version = data[8];
        let fragment_duration = if version == 1 {
            if size < 20 {
                return Err("Invalid MEHD v1 size".into());
            }
            u64::from_be_bytes(data[12..20].try_into().unwrap())
        } else if version == 0 {
            u32::from_be_bytes(data[12..16].try_into().unwrap()) as u64
        } else {
            return Err("Unsupported MEHD version".into());
        };

        Ok((
            MehdBox { version, fragment_duration },
            size
        ))
    }
}
