use crate::format_fourcc;

use super::{dref::DrefBox, generic::Mp4Box};

// The `DinfBox` struct represents a Data Information Box in the MP4 file format.
// It contains a single field `dref` which is a `DrefBox` (Data Reference Box).
// The `DinfBox` is responsible for holding information about the data references used in the file.
#[derive(Default, Clone)]
pub struct DinfBox {
    pub dref: DrefBox, // The `dref` field contains the data reference box.
}

impl std::fmt::Debug for DinfBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DinfBox")
            .field("box_size", &self.box_size())
            .field("box_type", &format_fourcc(&self.box_type()))
            .field("dref", &self.dref)
            .finish()
    }
}

// Implementation of the `Mp4Box` trait for the `DinfBox` struct.
impl Mp4Box for DinfBox {
    // Returns the box type as a 4-byte array. For `DinfBox`, the type is "dinf".
    fn box_type(&self) -> [u8; 4] { *b"dinf" }

    // Calculates the size of the `DinfBox` in bytes.
    // The size includes 8 bytes for the header (4 bytes for size and 4 bytes for type)
    // and the size of the contained `DrefBox`.
    fn box_size(&self) -> u32 {
        8 + self.dref.box_size()
    }

    // Writes the `DinfBox` to the provided buffer.
    // The method serializes the box size, box type, and the contained `DrefBox` into the buffer.
    fn write_box(&self, buffer: &mut Vec<u8>) {
        // Write the size of the box in big-endian format.
        buffer.extend_from_slice(&self.box_size().to_be_bytes());
        // Write the box type ("dinf").
        buffer.extend_from_slice(&self.box_type());
        // Write the contained `DrefBox`.
        let current_size = buffer.len();
        let dref_size = self.dref.box_size() as usize;
        self.dref.write_box(buffer);
        if buffer.len() != current_size + dref_size {
            panic!("Error writing DrefBox: expected size {}, got {}", dref_size, buffer.len() - current_size);
        }

    }

    // Reads a `DinfBox` from the provided dat buffer.
    // The method checks the size and type of the box, and then reads the contained `DrefBox`.
    // It returns a tuple containing the `DinfBox` and the size of the box.
    // If the data is incomplete or the type is incorrect, it returns an error.
    fn read_box(data: &[u8]) -> Result<(Self, usize), String> {
        // Read the size of the box from the first 4 bytes.
        let size = u32::from_be_bytes(data[0..4].try_into().unwrap()) as usize;
        // The size must be at least 8 bytes (4 for size and 4 for type).
        if size < 8 || data.len() < size {
            return Err("Incomplete DINF box".into());
        }
        // Check if the box type is "dinf".
        // The type is stored in the next 4 bytes.
        if &data[4..8] != b"dinf" {
            return Err("Not a DINF box".into());
        }

        // The next bytes inside this box should be a `DrefBox`.
        // Read the `DrefBox` from the data slice from byte 8 to the end of the box.
        let (dref_box, _dref_size) = DrefBox::read_box(&data[8..size])?;
        Ok((DinfBox { dref: dref_box }, size))
    }
}
