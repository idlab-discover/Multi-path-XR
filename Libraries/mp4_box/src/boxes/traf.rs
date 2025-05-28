use crate::format_fourcc;

use super::{generic::Mp4Box, tfdt::TfdtBox, tfhd::TfhdBox, trun::TrunBox};

// The `TrafBox` struct represents a Track Fragment Box in the MP4 file format.
// This box is used in fragmented MP4 files to group information about a track fragment.
// It contains the following sub-boxes:
// - `TfhdBox`: The Track Fragment Header Box, which provides information about the track fragment.
// - `TfdtBox`: The Track Fragment Decode Time Box, which specifies the decode time of the first sample in the fragment.
// - `TrunBox`: The Track Run Box, which specifies the samples in the track fragment.
//
// Fields:
// - `tfhd`: An instance of `TfhdBox` representing the track fragment header.
// - `tfdt`: An instance of `TfdtBox` representing the track fragment decode time.
// - `trun`: An instance of `TrunBox` representing the track run.
#[derive(Default, Clone)]
pub struct TrafBox { // Track Fragment Box
    pub tfhd: TfhdBox, // Track Fragment Header Box
    pub tfdt: Option<TfdtBox>, // Optional Track Fragment Decode Time Box
    pub trun: Option<TrunBox>, // Optional Track Run Box
}

impl std::fmt::Debug for TrafBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TrafBox")
            .field("box_size", &self.box_size())
            .field("box_type", &format_fourcc(&self.box_type()))
            .field("tfhd", &self.tfhd)
            .field("tfdt", &self.tfdt)
            .field("trun", &self.trun)
            .finish()
    }
}


// Implementation of the `Mp4Box` trait for the `TrafBox` struct.
impl Mp4Box for TrafBox {
    // Returns the box type as a 4-byte array. For `TrafBox`, the type is "traf".
    fn box_type(&self) -> [u8; 4] { *b"traf" }

    // Calculates the size of the `TrafBox` in bytes.
    // The size includes:
    // - 8 bytes for the header (4 bytes for size and 4 bytes for type).
    // - The size of the `TfhdBox`.
    // - The size of the `TfdtBox`.
    // - The size of the `TrunBox`.
    fn box_size(&self) -> u32 {
        let mut size = 8; // header
        size += self.tfhd.box_size();
        if let Some(ref tfdt) = self.tfdt {
            size += tfdt.box_size();
        }
        if let Some(ref trun) = self.trun {
            size += trun.box_size();
        }
        size
    }

    // Writes the `TrafBox` to the provided buffer.
    // The method serializes the box size, box type, and the contents of the `TfhdBox`,
    // `TfdtBox`, and `TrunBox` into the buffer.
    fn write_box(&self, buffer: &mut Vec<u8>) {
        // Write the size of the box in big-endian format.
        buffer.extend_from_slice(&self.box_size().to_be_bytes());
        // Write the box type ("traf").
        buffer.extend_from_slice(&self.box_type());
        // Write the contents of the `TfhdBox`.
        let current_size = buffer.len();
        let tfhd_size = self.tfhd.box_size() as usize;
        self.tfhd.write_box(buffer);
        if buffer.len() != current_size + tfhd_size {
            panic!("Error writing TfhdBox: expected size {}, got {}", tfhd_size, buffer.len() - current_size);
        }

        if let Some(ref tfdt) = self.tfdt {
            let current_size = buffer.len();
            let tfdt_size = tfdt.box_size() as usize;
            tfdt.write_box(buffer);
            if buffer.len() != current_size + tfdt_size {
                panic!("Error writing TfdtBox: expected size {}, got {}", tfdt_size, buffer.len() - current_size);
            }
        }

        if let Some(ref trun) = self.trun {
            let current_size = buffer.len();
            let trun_size = trun.box_size() as usize;
            trun.write_box(buffer);
            if buffer.len() != current_size + trun_size {
                panic!("Error writing TrunBox: expected size {}, got {}", trun_size, buffer.len() - current_size);
            }
        }
    }

    fn read_box(data: &[u8]) -> Result<(Self, usize), String> {
        let size = u32::from_be_bytes(data[0..4].try_into().unwrap()) as usize;
        if data.len() < size {
            return Err("Incomplete TRAF box".into());
        }
        if &data[4..8] != b"traf" {
            return Err("Not a TRAF box".into());
        }

        let mut offset = 8;
        let mut tfhd = None;
        let mut tfdt = None;
        let mut trun = None;

        while offset + 8 <= size {
            let sub_size = u32::from_be_bytes(data[offset..offset+4].try_into().unwrap()) as usize;
            let sub_type = &data[offset+4..offset+8];

            if offset + sub_size > size || sub_size < 8 {
                return Err("Invalid sub-box size inside TRAF".into());
            }

            match sub_type {
                b"tfhd" => {
                    if tfhd.is_some() {
                        return Err("Duplicate TFHD box inside TRAF".into());
                    }
                    let (parsed, parsed_size) = TfhdBox::read_box(&data[offset..offset+sub_size])?;
                    if parsed_size != sub_size {
                        return Err("Incorrect TFHD box size".into());
                    }
                    tfhd = Some(parsed);
                }
                b"tfdt" => {
                    if tfdt.is_some() {
                        return Err("Duplicate TFDT box inside TRAF".into());
                    }
                    let (parsed, parsed_size) = TfdtBox::read_box(&data[offset..offset+sub_size])?;
                    if parsed_size != sub_size {
                        return Err("Incorrect TFDT box size".into());
                    }
                    tfdt = Some(parsed);
                }
                b"trun" => {
                    if trun.is_some() {
                        return Err("Duplicate TRUN box inside TRAF".into());
                    }
                    let (parsed, parsed_size) = TrunBox::read_box(&data[offset..offset+sub_size])?;
                    if parsed_size != sub_size {
                        return Err("Incorrect TRUN box size".into());
                    }
                    trun = Some(parsed);
                }
                _ => {
                    // Skip unknown boxes
                }
            }

            offset += sub_size;
        }

        if tfhd.is_none() {
            return Err("Missing required TFHD box inside TRAF".into());
        }

        Ok((
            TrafBox {
                tfhd: tfhd.unwrap(),
                tfdt,
                trun,
            },
            size
        ))
    }
}
