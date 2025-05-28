use crate::format_fourcc;
use super::{generic::Mp4Box, meta::MetaBox};

/// The `UdtaBox` represents the User Data Box in the MP4 file format.
/// It typically contains user-specific data, often including a `MetaBox`.
#[derive(Default, Clone)]
pub struct UdtaBox {
    pub meta: Option<MetaBox>,  // Optional MetaBox inside UdtaBox
}

impl std::fmt::Debug for UdtaBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut dbg = f.debug_struct("UdtaBox");
        dbg.field("box_size", &self.box_size())
           .field("box_type", &format_fourcc(&self.box_type()));
        if self.meta.is_some() {
            dbg.field("meta", &"Present");
        } else {
            dbg.field("meta", &"None");
        }
        dbg.finish()
    }
}

impl Mp4Box for UdtaBox {
    fn box_type(&self) -> [u8; 4] { *b"udta" }

    fn box_size(&self) -> u32 {
        8 + self.meta.as_ref().map_or(0, |m| m.box_size())
    }

    fn write_box(&self, buffer: &mut Vec<u8>) {
        buffer.extend_from_slice(&self.box_size().to_be_bytes());
        buffer.extend_from_slice(&self.box_type());
        if let Some(meta_box) = &self.meta {
            meta_box.write_box(buffer);
        }
    }

    fn read_box(data: &[u8]) -> Result<(Self, usize), String> {
        if data.len() < 8 {
            return Err("UDTA box too small".into());
        }

        let size = u32::from_be_bytes(data[0..4].try_into().unwrap()) as usize;
        if data.len() < size {
            return Err("Incomplete UDTA box".into());
        }

        if &data[4..8] != b"udta" {
            return Err("Not a UDTA box".into());
        }

        let offset = 8;
        let mut meta = None;

        if offset < size {
            let box_type = &data[offset+4..offset+8];
            if box_type == b"meta" {
                let (meta_box, _consumed) = MetaBox::read_box(&data[offset..])?;
                meta = Some(meta_box);
                // offset += consumed;
            } else {
                return Err(format!("Unexpected box in UDTA: {:?}", box_type));
            }
        }

        Ok((UdtaBox { meta }, size))
    }
}
