use crate::format_fourcc;

use super::{generic::Mp4Box, meta::MetaBox, mvex::MvexBox, mvhd::MvhdBox, trak::TrakBox, udta::UdtaBox};

// The `MoovBox` struct represents a Movie Box in the MP4 file format.
// This box is a container for all the metadata related to the entire movie.
// It contains the following fields:
// - `mvhd`: An instance of `MvhdBox` representing the Movie Header Box, which contains global information about the movie.
// - `trak`: An instance of `TrakBox` representing the Track Box, which is a container for track-specific information.
// - `mvex`: An instance of `MvexBox` representing the Movie Extends Box, which provides information for movie fragments.
//
// The `MoovBox` is one of the most important boxes in the MP4 file format as it holds the structural and timing metadata for the entire movie.
#[derive(Default, Clone)]
pub struct MoovBox { // Compressed Movie Box
    pub mvhd: MvhdBox,             // Movie Header Box (mandatory)
    pub traks: Vec<TrakBox>,       // One or more Track Boxes
    pub mvex: Option<MvexBox>,     // Movie Extends Box (optional)
    pub meta: Option<MetaBox>,     // Metadata Box (optional)
    pub udta: Option<UdtaBox>,     // User Data Box (optional)
}

impl std::fmt::Debug for MoovBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut dbg = f.debug_struct("MoovBox");
        dbg.field("box_size", &self.box_size())
           .field("box_type", &format_fourcc(&self.box_type()))
           .field("traks", &self.traks);
        if self.mvex.is_some() { dbg.field("mvex", &self.mvex); }
        if self.meta.is_some() { dbg.field("meta", &self.meta); }
        if self.udta.is_some() { dbg.field("udta", &self.udta); }
        dbg.finish()
    }
}

// Implementation of the `Mp4Box` trait for the `MoovBox` struct.
impl Mp4Box for MoovBox {
    // Returns the box type as a 4-byte array. For `MoovBox`, the type is "moov".
    fn box_type(&self) -> [u8; 4] { *b"moov" }

    // Calculates the size of the `MoovBox` in bytes.
    // The size includes:
    // - 8 bytes for the header (4 bytes for size and 4 bytes for type).
    // - The size of the `MvhdBox`.
    // - The size of the `TrakBox`.
    // - The size of the `MvexBox`.
    fn box_size(&self) -> u32 {
        8 + self.mvhd.box_size() +
        self.traks.iter().map(|t| t.box_size()).sum::<u32>() +
        self.mvex.as_ref().map_or(0, |b| b.box_size()) +
        self.meta.as_ref().map_or(0, |b| b.box_size()) +
        self.udta.as_ref().map_or(0, |b| b.box_size())
    }

    // Writes the `MoovBox` to the provided buffer.
    // The method serializes the box size, box type, and the contents of the `MvhdBox`,
    // `TrakBox`, and `MvexBox` into the buffer.
    fn write_box(&self, buffer: &mut Vec<u8>) {
        // Write the size of the box in big-endian format.
        buffer.extend_from_slice(&self.box_size().to_be_bytes());
        // Write the box type ("moov").
        buffer.extend_from_slice(&self.box_type());
        let current_size = buffer.len();
        let mvhd_size = self.mvhd.box_size() as usize;
        self.mvhd.write_box(buffer);
        if buffer.len() != current_size + mvhd_size {
            panic!("Error writing MvhdBox: expected size {}, got {}", mvhd_size, buffer.len() - current_size);
        }
        for trak in &self.traks {
            let current_size = buffer.len();
            let trak_size = trak.box_size() as usize;
            trak.write_box(buffer);
            if buffer.len() != current_size + trak_size {
                panic!("Error writing TrakBox: expected size {}, got {}", trak_size, buffer.len() - current_size);
            }
        }
        if let Some(mvex) = &self.mvex {
            let current_size = buffer.len();
            let mvex_size = mvex.box_size() as usize;
            mvex.write_box(buffer);
            if buffer.len() != current_size + mvex_size {
                panic!("Error writing MvexBox: expected size {}, got {}", mvex_size, buffer.len() - current_size);
            }
        }
        if let Some(meta) = &self.meta {
            let current_size = buffer.len();
            let meta_size = meta.box_size() as usize;
            meta.write_box(buffer);
            if buffer.len() != current_size + meta_size {
                panic!("Error writing MetaBox: expected size {}, got {}", meta_size, buffer.len() - current_size);
            }
        }
        if let Some(udta) = &self.udta {
            let current_size = buffer.len();
            let udta_size = udta.box_size() as usize;
            udta.write_box(buffer);
            if buffer.len() != current_size + udta_size {
                panic!("Error writing UdtaBox: expected size {}, got {}", udta_size, buffer.len() - current_size);
            }
        }
    }

    fn read_box(data: &[u8]) -> Result<(Self, usize), String> {
        let size = u32::from_be_bytes(data[0..4].try_into().unwrap()) as usize;
        if data.len() < size {
            return Err("Incomplete MOOV box".into());
        }
        if &data[4..8] != b"moov" {
            return Err("Not a MOOV box".into());
        }

        let mut offset = 8;
        let (mvhd, mvhd_size) = MvhdBox::read_box(&data[offset..])?;
        offset += mvhd_size;

        let mut traks = Vec::new();
        let mut mvex = None;
        let mut meta = None;
        let mut udta = None;

        while offset < size {
            let box_type = &data[offset+4..offset+8];
            // let sub_box_size = u32::from_be_bytes(data[offset..offset+4].try_into().unwrap()) as usize;

            match box_type {
                b"trak" => {
                    let (trak, consumed) = TrakBox::read_box(&data[offset..])?;
                    traks.push(trak);
                    offset += consumed;
                }
                b"mvex" => {
                    let (parsed, consumed) = MvexBox::read_box(&data[offset..])?;
                    mvex = Some(parsed);
                    offset += consumed;
                }
                b"meta" => {
                    let (parsed, consumed) = MetaBox::read_box(&data[offset..])?;
                    meta = Some(parsed);
                    offset += consumed;
                }
                b"udta" => {
                    let (parsed, consumed) = UdtaBox::read_box(&data[offset..])?;
                    udta = Some(parsed);
                    offset += consumed;
                }
                _ => {
                    return Err(format!("Unknown box type in MOOV: {:?}", box_type));
                }
            }
        }

        if traks.is_empty() {
            return Err("MOOV box must contain at least one TRAK box".into());
        }

        Ok((
            MoovBox { mvhd, traks, mvex, meta, udta },
            size
        ))
    }
}
