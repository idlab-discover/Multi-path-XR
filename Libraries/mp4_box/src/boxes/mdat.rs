use crate::{format_capped_bytes, format_fourcc};

use super::generic::Mp4Box;

// The `MdatBox` struct represents a Media Data Box in the MP4 file format.
// This box contains the raw media data, such as video frames or audio samples.
// It is one of the most important boxes in the MP4 file format as it holds the actual media content.
//
// Fields:
// - `data`: A vector of bytes representing the raw encoded media data.
#[derive(Default, Clone)]
pub struct MdatBox { // Media Data Box
    pub data: Vec<u8>,   // The raw encoded frame
}

impl std::fmt::Debug for MdatBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MdatBox")
            .field("box_size", &self.box_size())
            .field("box_type", &format_fourcc(&self.box_type()))
            .field("data", &format_capped_bytes(&self.data))
            .finish()
    }
}

// Implementation of the `Mp4Box` trait for the `MdatBox` struct.
impl Mp4Box for MdatBox {
    // Returns the box type as a 4-byte array. For `MdatBox`, the type is "mdat".
    fn box_type(&self) -> [u8; 4] { *b"mdat" }

    // Calculates the size of the `MdatBox` in bytes.
    // The size includes:
    // - 8 bytes for the header (4 bytes for size and 4 bytes for type).
    // - The size of the `data` field, which contains the raw media data.
    fn box_size(&self) -> u32 {
        8 + self.data.len() as u32
    }

    // Writes the `MdatBox` to the provided buffer.
    // The method serializes the box size, box type, and the raw media data into the buffer.
    fn write_box(&self, buffer: &mut Vec<u8>) {
        // Write the size of the box in big-endian format.
        buffer.extend_from_slice(&self.box_size().to_be_bytes());
        // Write the box type ("mdat").
        buffer.extend_from_slice(&self.box_type());
        // Write the raw media data.
        buffer.extend_from_slice(&self.data);
    }

    fn read_box(data: &[u8]) -> Result<(Self, usize), String> {
        if data.len() < 8 {
            return Err("MDAT box too small".into());
        }

        let size = u32::from_be_bytes(data[0..4].try_into().unwrap()) as usize;
        let box_type = &data[4..8];
        if box_type != b"mdat" {
            return Err("Not an MDAT box".into());
        }

        if data.len() < size {
            return Err("Incomplete MDAT box".into());
        }

        let payload = data[8..size].to_vec();

        Ok((
            MdatBox { data: payload },
            size
        ))
    }
}
