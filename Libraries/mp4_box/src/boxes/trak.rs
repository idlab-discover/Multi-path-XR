use crate::format_fourcc;

use super::{edts::EdtsBox, generic::Mp4Box, mdia::MdiaBox, meta::MetaBox, tkhd::TkhdBox};

// The `TrakBox` struct represents a Track Box in the MP4 file format.
// This box is a container for all the information related to a single track in the movie.
// It contains the following sub-boxes:
// - `TkhdBox`: The Track Header Box, which contains metadata about the track, such as its ID, duration, and dimensions.
// - `MdiaBox`: The Media Box, which is a container for media-specific information, such as the media header and sample table.
//
// Fields:
// - `tkhd`: An instance of `TkhdBox` representing the track header.
// - `EdtsBox`: (Optional) The Edit Box specifying edit lists.
// - `MetaBox`: (Optional) Metadata specific to the track.
// - `mdia`: An instance of `MdiaBox` representing the media information.
#[derive(Default, Clone)]
pub struct TrakBox { // Track Box
    pub tkhd: TkhdBox, // Track Header Box
    pub edts: Option<EdtsBox>, // Optional Edit Box
    pub meta: Option<MetaBox>, // Optional Metadata Box
    pub mdia: MdiaBox, // Media Box
}

impl std::fmt::Debug for TrakBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TrakBox")
            .field("box_size", &self.box_size())
            .field("box_type", &format_fourcc(&self.box_type()))
            .field("tkhd", &self.tkhd)
            .field("edts", &self.edts)
            .field("meta", &self.meta)
            .field("mdia", &self.mdia)
            .finish()
    }
}


// Implementation of the `Mp4Box` trait for the `TrakBox` struct.
impl Mp4Box for TrakBox {
    // Returns the box type as a 4-byte array. For `TrakBox`, the type is "trak".
    fn box_type(&self) -> [u8; 4] { *b"trak" }

    // Calculates the size of the `TrakBox` in bytes.
    // The size includes:
    // - 8 bytes for the header (4 bytes for size and 4 bytes for type).
    // - The size of the `TkhdBox`.
    // - The size of the `MdiaBox`.
    fn box_size(&self) -> u32 {
        let mut size = 8; // header
        size += self.tkhd.box_size();
        if let Some(ref edts) = self.edts {
            size += edts.box_size();
        }
        if let Some(ref meta) = self.meta {
            size += meta.box_size();
        }
        size += self.mdia.box_size();
        size
    }

    // Writes the `TrakBox` to the provided buffer.
    // The method serializes the box size, box type, and the contents of the `TkhdBox` and `MdiaBox` into the buffer.
    fn write_box(&self, buffer: &mut Vec<u8>) {
        buffer.extend_from_slice(&self.box_size().to_be_bytes());
        buffer.extend_from_slice(&self.box_type());

        let current_size = buffer.len();
        let tkhd_size = self.tkhd.box_size() as usize;
        self.tkhd.write_box(buffer);
        if buffer.len() != current_size + tkhd_size {
            panic!("Error writing TkhdBox: expected size {}, got {}", tkhd_size, buffer.len() - current_size);
        }

        if let Some(ref edts) = self.edts {
            let current_size = buffer.len();
            let edts_size = edts.box_size() as usize;
            edts.write_box(buffer);
            if buffer.len() != current_size + edts_size {
                panic!("Error writing EdtsBox: expected size {}, got {}", edts_size, buffer.len() - current_size);
            }
        }

        if let Some(ref meta) = self.meta {
            let current_size = buffer.len();
            let meta_size = meta.box_size() as usize;
            meta.write_box(buffer);
            if buffer.len() != current_size + meta_size {
                panic!("Error writing MetaBox: expected size {}, got {}", meta_size, buffer.len() - current_size);
            }
        }

        let current_size = buffer.len();
        let mdia_size = self.mdia.box_size() as usize;
        self.mdia.write_box(buffer);
        if buffer.len() != current_size + mdia_size {
            panic!("Error writing MdiaBox: expected size {}, got {}", mdia_size, buffer.len() - current_size);
        }
    }

    fn read_box(data: &[u8]) -> Result<(Self, usize), String> {
        let size = u32::from_be_bytes(data[0..4].try_into().unwrap()) as usize;
        if data.len() < size {
            return Err("Incomplete TRAK box".into());
        }
        if &data[4..8] != b"trak" {
            return Err("Not a TRAK box".into());
        }

        let mut offset = 8;
        let mut tkhd = None;
        let mut edts = None;
        let mut meta = None;
        let mut mdia = None;

        while offset + 8 <= size {
            let sub_size = u32::from_be_bytes(data[offset..offset+4].try_into().unwrap()) as usize;
            let sub_type = &data[offset+4..offset+8];

            if offset + sub_size > size || sub_size < 8 {
                return Err("Invalid sub-box size inside TRAK".into());
            }

            match sub_type {
                b"tkhd" => {
                    if tkhd.is_some() {
                        return Err("Duplicate TKHD box inside TRAK".into());
                    }
                    let (parsed, parsed_size) = TkhdBox::read_box(&data[offset..offset+sub_size])?;
                    if parsed_size != sub_size {
                        return Err("Incorrect TKHD box size".into());
                    }
                    tkhd = Some(parsed);
                }
                b"edts" => {
                    if edts.is_some() {
                        return Err("Duplicate EDTS box inside TRAK".into());
                    }
                    let (parsed, parsed_size) = EdtsBox::read_box(&data[offset..offset+sub_size])?;
                    if parsed_size != sub_size {
                        return Err("Incorrect EDTS box size".into());
                    }
                    edts = Some(parsed);
                }
                b"meta" => {
                    if meta.is_some() {
                        return Err("Duplicate META box inside TRAK".into());
                    }
                    let (parsed, parsed_size) = MetaBox::read_box(&data[offset..offset+sub_size])?;
                    if parsed_size != sub_size {
                        return Err("Incorrect META box size".into());
                    }
                    meta = Some(parsed);
                }
                b"mdia" => {
                    if mdia.is_some() {
                        return Err("Duplicate MDIA box inside TRAK".into());
                    }
                    let (parsed, parsed_size) = MdiaBox::read_box(&data[offset..offset+sub_size])?;
                    if parsed_size != sub_size {
                        return Err("Incorrect MDIA box size".into());
                    }
                    mdia = Some(parsed);
                }
                _ => {
                    // Skip unknown boxes safely
                }
            }

            offset += sub_size;
        }

        if tkhd.is_none() {
            return Err("Missing required TKHD box inside TRAK".into());
        }
        if mdia.is_none() {
            return Err("Missing required MDIA box inside TRAK".into());
        }

        Ok((
            TrakBox {
                tkhd: tkhd.unwrap(),
                edts,
                meta,
                mdia: mdia.unwrap(),
            },
            size
        ))
    }
}
