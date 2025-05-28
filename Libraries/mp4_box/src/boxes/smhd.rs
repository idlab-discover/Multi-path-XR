use crate::format_fourcc;
use super::generic::Mp4Box;

/// The `SmhdBox` represents the Sound Media Header Box.
/// It provides audio-specific information, like balance.
#[derive(Clone)]
pub struct SmhdBox {
    pub version: u8,
    pub flags: u32,
    pub balance: i16,  // 8.8 fixed-point format
}

impl Default for SmhdBox {
    fn default() -> Self {
        SmhdBox {
            version: 0,
            flags: 0,
            balance: 0,  // Centered audio
        }
    }
}

impl std::fmt::Debug for SmhdBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SmhdBox")
            .field("box_size", &self.box_size())
            .field("box_type", &format_fourcc(&self.box_type()))
            .field("version", &self.version)
            .field("flags", &format!("0x{:06X}", self.flags))
            .field("balance", &self.balance)
            .finish()
    }
}

impl Mp4Box for SmhdBox {
    fn box_type(&self) -> [u8; 4] { *b"smhd" }

    fn box_size(&self) -> u32 {
        8 + 4 + 4  // Header + version/flags + balance/reserved
    }

    fn write_box(&self, buffer: &mut Vec<u8>) {
        buffer.extend_from_slice(&self.box_size().to_be_bytes());
        buffer.extend_from_slice(&self.box_type());
        buffer.push(self.version);
        buffer.extend_from_slice(&(self.flags & 0x00FFFFFF).to_be_bytes()[1..]);
        buffer.extend_from_slice(&self.balance.to_be_bytes());
        buffer.extend_from_slice(&0u16.to_be_bytes());  // reserved
    }

    fn read_box(data: &[u8]) -> Result<(Self, usize), String> {
        if data.len() < 16 {
            return Err("SMHD box too small".into());
        }

        let size = u32::from_be_bytes(data[0..4].try_into().unwrap()) as usize;
        if size != 16 {
            return Err("Invalid SMHD box size".into());
        }

        if &data[4..8] != b"smhd" {
            return Err("Not an SMHD box".into());
        }

        let version = data[8];
        let flags = u32::from_be_bytes([0, data[9], data[10], data[11]]);
        let balance = i16::from_be_bytes(data[12..14].try_into().unwrap());

        Ok((
            SmhdBox {
                version,
                flags,
                balance,
            },
            size
        ))
    }
}
