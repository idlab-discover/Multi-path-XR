use crate::{format_capped_bytes, format_fourcc};

// The `Mp4Box` trait defines a generic interface for MP4 boxes.
// MP4 boxes are the fundamental building blocks of the MP4 file format.
// Each box has a specific type, size, and content, and this trait provides
// methods to interact with these properties.
//
// Required Methods:
// - `box_type`: Returns the 4-byte type identifier of the box.
// - `box_size`: Calculates the total size of the box in bytes, including the header.
// - `write_box`: Serializes the box into a buffer for writing to a file or stream.
//
// Implementing this trait allows a struct to represent a specific type of MP4 box
// and ensures that it can be serialized and identified correctly.
pub trait Mp4Box {
    // Returns the 4-byte type identifier of the box.
    // This identifier is used to distinguish different types of MP4 boxes.
    fn box_type(&self) -> [u8; 4];

    // Calculates the total size of the box in bytes.
    // The size includes the header (8 bytes: 4 bytes for size and 4 bytes for type)
    // and the size of the box's content.
    fn box_size(&self) -> u32;

    // Serializes the box into the provided buffer.
    // The method writes the box's size, type, and content into the buffer
    // in the correct format for MP4 files.
    fn write_box(&self, buffer: &mut Vec<u8>);

    /// Reads a box from the given byte slice.
    /// Returns a tuple of (BoxInstance, bytes_consumed).
    fn read_box(data: &[u8]) -> Result<(Self, usize), String> where Self: Sized;
}


// The `UnknownBox` struct represents a box in the MP4 file format that we haven't implemented yet.
// This box contains the raw data that is included inside this box.
//
// Fields:
// - `data`: A vector of bytes representing the raw data.
#[derive(Clone)]
pub struct UnknownBox { // Media Data Box
    pub btype: [u8; 4], // The type of the box (4 bytes)
    pub data: Vec<u8>,   // The raw encoded frame
}

impl Default for UnknownBox {
    fn default() -> Self {
        UnknownBox {
            btype: *b"xxxx",   // By default, the box type is set to "xxxx"
            data: Vec::new(), // Initialize the data vector as empty
        }
    }
}

impl std::fmt::Debug for UnknownBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MdatBox")
            .field("box_size", &self.box_size())
            .field("box_type", &format_fourcc(&self.box_type()))
            .field("data", &format_capped_bytes(&self.data))
            .finish()
    }
}

// Implementation of the `Mp4Box` trait for the `MdatBox` struct.
impl Mp4Box for UnknownBox {
    // Returns the box type as a 4-byte array. For `UnknownBox`, the type is "xxxx" by default.
    // However, when reading from a file, this type can be set to the actual box type.
    // This allows the `UnknownBox` to represent any box type that is not explicitly defined.
    fn box_type(&self) -> [u8; 4] {
        self.btype
    }

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
            return Err("Unknown box too small".into());
        }

        let size = u32::from_be_bytes(data[0..4].try_into().unwrap()) as usize;
        let box_type = &data[4..8];

        if data.len() < size {
            return Err("Incomplete unknown box".into());
        }

        let payload = data[8..size].to_vec();

        Ok((
            UnknownBox {
                btype: box_type.try_into().map_err(|_| "Invalid box type length")?,
                data: payload
            },
            size
        ))
    }
}

