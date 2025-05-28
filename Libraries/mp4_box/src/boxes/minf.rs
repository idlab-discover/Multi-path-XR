use crate::format_fourcc;

use super::{dinf::DinfBox, generic::Mp4Box, smhd::SmhdBox, stbl::StblBox, vmhd::VmhdBox};

// The `MinfBox` struct represents a Media Information Box in the MP4 file format.
// This box is a container for media-specific information and includes the following sub-boxes:
// - `VmhdBox`: The Video Media Header Box, which contains video-specific information.
// - `DinfBox`: The Data Information Box, which provides information about data references.
// - `StblBox`: The Sample Table Box, which contains detailed information about the media samples.
//
// Fields:
// - `vmhd`: An instance of `VmhdBox` representing the video media header.
// - `dinf`: An instance of `DinfBox` representing the data information.
// - `stbl`: An instance of `StblBox` representing the sample table.
#[derive(Default, Clone)]
pub struct MinfBox { // Media Information Box
    pub vmhd: Option<VmhdBox>,  // Video Media Header Box (optional)
    pub smhd: Option<SmhdBox>,  // Sound Media Header Box (optional)
    pub dinf: DinfBox, // Data Information Box
    pub stbl: StblBox, // Sample Table Box
}

impl std::fmt::Debug for MinfBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut dbg = f.debug_struct("MinfBox");
        dbg.field("box_size", &self.box_size())
           .field("box_type", &format_fourcc(&self.box_type()));
        if let Some(vmhd) = &self.vmhd {
            dbg.field("vmhd", vmhd);
        }
        if let Some(smhd) = &self.smhd {
            dbg.field("smhd", smhd);
        }
        dbg.field("dinf", &self.dinf)
           .field("stbl", &self.stbl)
           .finish()
    }
}

// Implementation of the `Mp4Box` trait for the `MinfBox` struct.
impl Mp4Box for MinfBox {
    // Returns the box type as a 4-byte array. For `MinfBox`, the type is "minf".
    fn box_type(&self) -> [u8; 4] { *b"minf" }

    // Calculates the size of the `MinfBox` in bytes.
    // The size includes:
    // - 8 bytes for the header (4 bytes for size and 4 bytes for type).
    // - The size of the `VmhdBox` or 'SmhdBox'.
    // - The size of the `DinfBox`.
    // - The size of the `StblBox`.
    fn box_size(&self) -> u32 {
        8 + 
        self.vmhd.as_ref().map_or(0, |b| b.box_size()) +
        self.smhd.as_ref().map_or(0, |b| b.box_size()) +
        self.dinf.box_size() +
        self.stbl.box_size()
    }

    // Writes the `MinfBox` to the provided buffer.
    // The method serializes the box size, box type, and the contents of the `VmhdBox` or 'SmhdBox',
    // `DinfBox`, and `StblBox` into the buffer.
    fn write_box(&self, buffer: &mut Vec<u8>) {
        // Write the size of the box in big-endian format.
        buffer.extend_from_slice(&self.box_size().to_be_bytes());
        // Write the box type ("minf").
        buffer.extend_from_slice(&self.box_type());
        // Write the contents of the `VmhdBox` or 'SmhdBox'.
        if let Some(vmhd) = &self.vmhd {
            let current_size = buffer.len();
            let vmhd_size = vmhd.box_size() as usize;
            vmhd.write_box(buffer);
            if buffer.len() != current_size + vmhd_size {
                panic!("Error writing VmhdBox: expected size {}, got {}", vmhd_size, buffer.len() - current_size);
            }
        }
        if let Some(smhd) = &self.smhd {
            let current_size = buffer.len();
            let smhd_size = smhd.box_size() as usize;
            smhd.write_box(buffer);
            if buffer.len() != current_size + smhd_size {
                panic!("Error writing SmhdBox: expected size {}, got {}", smhd_size, buffer.len() - current_size);
            }
        }
        // Write the contents of the `DinfBox`.
        let current_size = buffer.len();
        let dinf_size = self.dinf.box_size() as usize;
        self.dinf.write_box(buffer);
        if buffer.len() != current_size + dinf_size {
            panic!("Error writing DinfBox: expected size {}, got {}", dinf_size, buffer.len() - current_size);
        }
        // Write the contents of the `StblBox`.
        let current_size = buffer.len();
        let stbl_size = self.stbl.box_size() as usize;
        self.stbl.write_box(buffer);
        if buffer.len() != current_size + stbl_size {
            panic!("Error writing StblBox: expected size {}, got {}", stbl_size, buffer.len() - current_size);
        }
    }

    fn read_box(data: &[u8]) -> Result<(Self, usize), String> {
        let size = u32::from_be_bytes(data[0..4].try_into().unwrap()) as usize;
        if data.len() < size {
            return Err("Incomplete MINF box".into());
        }
        if &data[4..8] != b"minf" {
            return Err("Not a MINF box".into());
        }

        let mut offset = 8;
        let mut vmhd = None;
        let mut smhd = None;
        let mut dinf = None;
        let mut stbl = None;

        while offset < size {
            let box_type = &data[offset+4..offset+8];
            // let sub_box_size = u32::from_be_bytes(data[offset..offset+4].try_into().unwrap()) as usize;

            match box_type {
                b"vmhd" => {
                    let (box_parsed, consumed) = VmhdBox::read_box(&data[offset..])?;
                    vmhd = Some(box_parsed);
                    offset += consumed;
                }
                b"smhd" => {
                    let (box_parsed, consumed) = SmhdBox::read_box(&data[offset..])?;
                    smhd = Some(box_parsed);
                    offset += consumed;
                }
                b"dinf" => {
                    let (box_parsed, consumed) = DinfBox::read_box(&data[offset..])?;
                    dinf = Some(box_parsed);
                    offset += consumed;
                }
                b"stbl" => {
                    let (box_parsed, consumed) = StblBox::read_box(&data[offset..])?;
                    stbl = Some(box_parsed);
                    offset += consumed;
                }
                _ => {
                    return Err(format!("Unknown box type in MINF: {:?}", box_type));
                }
            }
        }

        if dinf.is_none() || stbl.is_none() {
            return Err("MINF missing mandatory dinf or stbl box".into());
        }

        Ok((
            MinfBox {
                vmhd,
                smhd,
                dinf: dinf.unwrap(),
                stbl: stbl.unwrap(),
            },
            size
        ))
    }
}
