use crate::format_fourcc;

use super::{generic::Mp4Box, hdlr::HdlrBox, mdhd::MdhdBox, minf::MinfBox};

// The `MdiaBox` struct represents a Media Box in the MP4 file format.
// This box is a container for media information and includes the following sub-boxes:
// - `MdhdBox`: The Media Header Box, which contains metadata about the media, such as timescale and duration.
// - `HdlrBox`: The Handler Reference Box, which specifies the type of media (e.g., video, audio) and provides a handler name.
// - `MinfBox`: The Media Information Box, which contains detailed information about the media data.
//
// Fields:
// - `mdhd`: An instance of `MdhdBox` representing the media header.
// - `hdlr`: An instance of `HdlrBox` representing the handler reference.
// - `minf`: An instance of `MinfBox` representing the media information.
#[derive(Default, Clone)]
pub struct MdiaBox { // Media Box
    pub mdhd: MdhdBox, // Media Header Box
    pub hdlr: HdlrBox, // Handler Reference Box
    pub minf: MinfBox, // Media Information Box
}

impl std::fmt::Debug for MdiaBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MdiaBox")
            .field("box_size", &self.box_size())
            .field("box_type", &format_fourcc(&self.box_type()))
            .field("mdhd", &self.mdhd)
            .field("hdlr", &self.hdlr)
            .field("minf", &self.minf)
            .finish()
    }
}


// Implementation of the `Mp4Box` trait for the `MdiaBox` struct.
impl Mp4Box for MdiaBox {
    // Returns the box type as a 4-byte array. For `MdiaBox`, the type is "mdia".
    fn box_type(&self) -> [u8; 4] { *b"mdia" }

    // Calculates the size of the `MdiaBox` in bytes.
    // The size includes:
    // - 8 bytes for the header (4 bytes for size and 4 bytes for type).
    // - The size of the `MdhdBox`.
    // - The size of the `HdlrBox`.
    // - The size of the `MinfBox`.
    fn box_size(&self) -> u32 {
        8 + self.mdhd.box_size() + self.hdlr.box_size() + self.minf.box_size()
    }

    // Writes the `MdiaBox` to the provided buffer.
    // The method serializes the box size, box type, and the contents of the `MdhdBox`,
    // `HdlrBox`, and `MinfBox` into the buffer.
    fn write_box(&self, buffer: &mut Vec<u8>) {
        // Write the size of the box in big-endian format.
        buffer.extend_from_slice(&self.box_size().to_be_bytes());
        // Write the box type ("mdia").
        buffer.extend_from_slice(&self.box_type());
        // Write the contents of the `MdhdBox`.
        let current_size = buffer.len();
        let mdhd_size = self.mdhd.box_size() as usize;
        self.mdhd.write_box(buffer);
        if buffer.len() != current_size + mdhd_size {
            panic!("Error writing MdhdBox: expected size {}, got {}", mdhd_size, buffer.len() - current_size);
        }
        // Write the contents of the `HdlrBox`.
        let current_size = buffer.len();
        let hdlr_size = self.hdlr.box_size() as usize;
        self.hdlr.write_box(buffer);
        if buffer.len() != current_size + hdlr_size {
            panic!("Error writing HdlrBox: expected size {}, got {}", hdlr_size, buffer.len() - current_size);
        }
        // Write the contents of the `MinfBox`.
        let current_size = buffer.len();
        let minf_size = self.minf.box_size() as usize;
        self.minf.write_box(buffer);
        if buffer.len() != current_size + minf_size {
            panic!("Error writing MinfBox: expected size {}, got {}", minf_size, buffer.len() - current_size);
        }
    }

    fn read_box(data: &[u8]) -> Result<(Self, usize), String> {
        let size = u32::from_be_bytes(data[0..4].try_into().unwrap()) as usize;
        if data.len() < size {
            return Err("Incomplete MDIA box".into());
        }
        if &data[4..8] != b"mdia" {
            return Err("Not a MDIA box".into());
        }

        let mut offset = 8;

        let (mdhd, mdhd_size) = MdhdBox::read_box(&data[offset..])?;
        offset += mdhd_size;

        let (hdlr, hdlr_size) = HdlrBox::read_box(&data[offset..])?;
        offset += hdlr_size;

        let (minf, _minf_size) = MinfBox::read_box(&data[offset..])?;
        //offset += minf_size;

        Ok((
            MdiaBox { mdhd, hdlr, minf },
            size
        ))
    }
}
