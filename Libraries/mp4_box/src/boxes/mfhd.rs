use crate::format_fourcc;

use super::generic::Mp4Box;

// The `MfhdBox` struct represents a Movie Fragment Header Box in the MP4 file format.
// This box is used in fragmented MP4 files to provide information about the movie fragment.
// It contains the following field:
// - `sequence_number`: A 32-bit unsigned integer that specifies the sequence number of the movie fragment.
//   This value typically starts at 1 and increments with each subsequent fragment.
#[derive(Clone)]
pub struct MfhdBox {
    pub version: u8,
    pub flags: u32,
    pub sequence_number: u32, // Sequence number of the movie fragment.
}

// Provides a default implementation for the `MfhdBox` struct.
// The default `MfhdBox` has the following value:
// - `sequence_number`: 1 (indicating the first movie fragment).
impl Default for MfhdBox {
    fn default() -> Self {
        MfhdBox {
            version: 0,
            flags: 0,
            sequence_number: 1,  // Typically starts at 1
        }
    }
}

impl std::fmt::Debug for MfhdBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MfhdBox")
            .field("box_size", &self.box_size())
            .field("box_type", &format_fourcc(&self.box_type()))
            .field("version", &self.version)
            .field("flags", &format!("0x{:06X}", self.flags))
            .field("sequence_number", &self.sequence_number)
            .finish()
    }
}


// Implementation of the `Mp4Box` trait for the `MfhdBox` struct.
impl Mp4Box for MfhdBox {
    // Returns the box type as a 4-byte array. For `MfhdBox`, the type is "mfhd".
    fn box_type(&self) -> [u8; 4] { *b"mfhd" }

    // Calculates the size of the `MfhdBox` in bytes.
    // The size is fixed at 16 bytes, which includes:
    // - 8 bytes for the header (4 bytes for size and 4 bytes for type).
    // - 4 bytes for the version and flags.
    // - 4 bytes for the `sequence_number` field.
    fn box_size(&self) -> u32 {
        8 + 4 + 4  // Header + version/flags + sequence_number
    }

    // Writes the `MfhdBox` to the provided buffer.
    // The method serializes the box size, box type, version, flags, and `sequence_number` into the buffer.
    fn write_box(&self, buffer: &mut Vec<u8>) {
        // Write the size of the box in big-endian format.
        buffer.extend_from_slice(&self.box_size().to_be_bytes());
        // Write the box type ("mfhd").
        buffer.extend_from_slice(&self.box_type());
        // Write the version (1 byte) and flags (3 bytes).
        buffer.push(self.version);
        buffer.extend_from_slice(&(self.flags & 0x00FFFFFF).to_be_bytes()[1..]);  // 3-byte flags
        // Write the `sequence_number` field in big-endian format.
        buffer.extend_from_slice(&self.sequence_number.to_be_bytes());
    }

    fn read_box(data: &[u8]) -> Result<(Self, usize), String> {
        if data.len() < 16 {
            return Err("MFHD box too small".into());
        }

        let size = u32::from_be_bytes(data[0..4].try_into().unwrap()) as usize;
        if &data[4..8] != b"mfhd" {
            return Err("Not an MFHD box".into());
        }

        let version = data[8];
        let flags = u32::from_be_bytes([0, data[9], data[10], data[11]]);
        let sequence_number = u32::from_be_bytes(data[12..16].try_into().unwrap());

        Ok((
            MfhdBox {
                version,
                flags,
                sequence_number,
            },
            size
        ))
    }
}
