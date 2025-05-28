use crate::format_fourcc;

use super::{generic::Mp4Box, mfhd::MfhdBox, traf::TrafBox};

// The `MoofBox` struct represents a Movie Fragment Box in the MP4 file format.
// This box is used in fragmented MP4 files to group a movie fragment header and one or more track fragments.
// It contains the following fields:
// - `mfhd`: An instance of `MfhdBox` representing the Movie Fragment Header Box.
// - `traf`: An instance of `TrafBox` representing the Track Fragment Box.
//
// The `MoofBox` is essential for enabling fragmented MP4 playback, where media data is split into multiple fragments.
#[derive(Default, Clone)]
pub struct MoofBox { // Movie Fragment Box
    pub mfhd: MfhdBox, // Movie Fragment Header Box
    pub trafs: Vec<TrafBox>, // One or more Track Fragment Boxes
}

impl std::fmt::Debug for MoofBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MoofBox")
            .field("box_size", &self.box_size())
            .field("box_type", &format_fourcc(&self.box_type()))
            .field("mfhd", &self.mfhd)
            .field("trafs", &self.trafs)
            .finish()
    }
}

// Implementation of the `Mp4Box` trait for the `MoofBox` struct.
impl Mp4Box for MoofBox {
    // Returns the box type as a 4-byte array. For `MoofBox`, the type is "moof".
    fn box_type(&self) -> [u8; 4] { *b"moof" }

    // Calculates the size of the `MoofBox` in bytes.
    // The size includes:
    // - 8 bytes for the header (4 bytes for size and 4 bytes for type).
    // - The size of the `MfhdBox`.
    // - The size of the `TrafBox`.
    fn box_size(&self) -> u32 {
        8 + self.mfhd.box_size() + self.trafs.iter().map(|t| t.box_size()).sum::<u32>()
    }

    // Writes the `MoofBox` to the provided buffer.
    // The method serializes the box size, box type, and the contents of the `MfhdBox` and `TrafBox` into the buffer.
    fn write_box(&self, buffer: &mut Vec<u8>) {
        // Write the size of the box in big-endian format.
        buffer.extend_from_slice(&self.box_size().to_be_bytes());
        // Write the box type ("moof").
        buffer.extend_from_slice(&self.box_type());
        // Write the contents of the `MfhdBox`.
        self.mfhd.write_box(buffer);
        // Write the contents of the `TrafBox` vector.
        for traf in &self.trafs {
            let current_size = buffer.len();
            let traf_size = traf.box_size() as usize;
            traf.write_box(buffer);
            if buffer.len() != current_size + traf_size {
                panic!("Error writing TrafBox: expected size {}, got {}", traf_size, buffer.len() - current_size);
            }
        }
    }

    fn read_box(data: &[u8]) -> Result<(Self, usize), String> {
        let size = u32::from_be_bytes(data[0..4].try_into().unwrap()) as usize;
        if data.len() < size {
            return Err("Incomplete MOOF box".into());
        }

        if &data[4..8] != b"moof" {
            return Err("Not a MOOF box".into());
        }

        let mut offset = 8;

        let (mfhd, mfhd_size) = MfhdBox::read_box(&data[offset..])?;
        offset += mfhd_size;

        let mut trafs = Vec::new();
        while offset < size {
            let box_type = &data[offset+4..offset+8];
            if box_type != b"traf" {
                return Err(format!("Unexpected box type in MOOF: {:?}", box_type));
            }
            let (traf, traf_size) = TrafBox::read_box(&data[offset..])?;
            trafs.push(traf);
            offset += traf_size;
        }

        if trafs.is_empty() {
            return Err("MOOF box must contain at least one TRAF box".into());
        }

        Ok((
            MoofBox { mfhd, trafs },
            size
        ))
    }
}
