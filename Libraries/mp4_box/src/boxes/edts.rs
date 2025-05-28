use crate::format_fourcc;

use super::{generic::Mp4Box, elst::ElstBox};

// The `EdtsBox` struct represents an Edit Box (`edts`) in the MP4 file format.
// This box contains information about how to map the media time-line to the presentation time-line.
// It mainly contains:
// - `ElstBox`: An Edit List Box specifying edit segments.
//
// Fields:
// - `elst`: An optional `ElstBox` containing edit list entries.
#[derive(Default, Clone)]
pub struct EdtsBox { // Edit Box
    pub elst: Option<ElstBox>, // Optional Edit List Box
}

impl std::fmt::Debug for EdtsBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EdtsBox")
            .field("box_size", &self.box_size())
            .field("box_type", &format_fourcc(&self.box_type()))
            .field("elst", &self.elst)
            .finish()
    }
}

// Implementation of the `Mp4Box` trait for the `EdtsBox` struct.
impl Mp4Box for EdtsBox {
    fn box_type(&self) -> [u8; 4] { *b"edts" }

    fn box_size(&self) -> u32 {
        let mut size = 8; // header
        if let Some(ref elst) = self.elst {
            size += elst.box_size();
        }
        size
    }

    fn write_box(&self, buffer: &mut Vec<u8>) {
        buffer.extend_from_slice(&self.box_size().to_be_bytes());
        buffer.extend_from_slice(&self.box_type());

        if let Some(ref elst) = self.elst {
            let current_size = buffer.len();
            let elst_size = elst.box_size() as usize;
            elst.write_box(buffer);
            if buffer.len() != current_size + elst_size {
                panic!("Error writing ElstBox: expected size {}, got {}", elst_size, buffer.len() - current_size);
            }
        }
    }

    fn read_box(data: &[u8]) -> Result<(Self, usize), String> {
        let size = u32::from_be_bytes(data[0..4].try_into().unwrap()) as usize;
        if data.len() < size {
            return Err("Incomplete EDTS box".into());
        }
        if &data[4..8] != b"edts" {
            return Err("Not an EDTS box".into());
        }

        let mut offset = 8;
        let mut elst = None;

        while offset + 8 <= size {
            let sub_size = u32::from_be_bytes(data[offset..offset+4].try_into().unwrap()) as usize;
            let sub_type = &data[offset+4..offset+8];

            if offset + sub_size > size || sub_size < 8 {
                return Err("Invalid sub-box inside EDTS".into());
            }

            match sub_type {
                b"elst" => {
                    if elst.is_some() {
                        return Err("Duplicate ELST box inside EDTS".into());
                    }
                    let (parsed, parsed_size) = ElstBox::read_box(&data[offset..offset+sub_size])?;
                    if parsed_size != sub_size {
                        return Err("Incorrect ELST box size".into());
                    }
                    elst = Some(parsed);
                }
                _ => {
                    // Unknown box under EDTS, safe to ignore
                }
            }

            offset += sub_size;
        }

        Ok((EdtsBox { elst }, size))
    }
}
