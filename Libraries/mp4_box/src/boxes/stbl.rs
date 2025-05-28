use crate::format_fourcc;

use super::{co64::Co64Box, ctts::CttsBox, generic::Mp4Box, stco::StcoBox, stsc::StscBox, stsd::StsdBox, stss::StssBox, stsz::StszBox, stts::SttsBox};

// The `StblBox` struct represents a Sample Table Box in the MP4 file format.
// This box is a container for all the time and data indexing of the media samples.
// It contains the following sub-boxes:
// - `StsdBox`: The Sample Description Box, which describes the format of the media samples.
// - `SttsBox`: The Time-to-Sample Box, which maps decoding times to samples.
// - `StscBox`: The Sample-to-Chunk Box, which maps samples to chunks.
// - `StszBox`: The Sample Size Box, which specifies the size of each sample.
// - `StcoBox`: The Chunk Offset Box, which specifies the location of chunks in the media data.
//
// The `StblBox` is essential for enabling efficient access to media samples and their associated metadata.
#[derive(Default, Clone)]
pub struct StblBox { // Sample Table Box
    pub stsd: StsdBox,
    pub stts: SttsBox,
    pub ctts: Option<CttsBox>,
    pub stss: Option<StssBox>,
    pub stsc: StscBox,
    pub stsz: StszBox,
    pub stco: Option<StcoBox>,
    pub co64: Option<Co64Box>,
}

impl std::fmt::Debug for StblBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StblBox")
            .field("box_size", &self.box_size())
            .field("box_type", &format_fourcc(&self.box_type()))
            .field("stsd", &self.stsd)
            .field("stsc", &self.stsc)
            .field("stsz", &self.stsz)
            .field("stco", &self.stco)
            .field("co64", &self.co64)
            .finish()
    }
}

// Implementation of the `Mp4Box` trait for the `StblBox` struct.
impl Mp4Box for StblBox {
    // Returns the box type as a 4-byte array. For `StblBox`, the type is "stbl".
    fn box_type(&self) -> [u8; 4] { *b"stbl" }

    // Calculates the size of the `StblBox` in bytes.
    // The size includes:
    // - 8 bytes for the header (4 bytes for size and 4 bytes for type).
    // - The size of all the sub-boxes:
    //   - `StsdBox`
    //   - `SttsBox`
    //   - `CttsBox` (optional)
    //   - `StssBox` (optional)
    //   - `StscBox`
    //   - `StszBox`
    //   - `StcoBox` (optional)
    //   - `Co64Box` (optional)
    fn box_size(&self) -> u32 {
        8 + self.stsd.box_size()
          + self.stts.box_size()
          + self.ctts.as_ref().map_or(0, |b| b.box_size())
          + self.stss.as_ref().map_or(0, |b| b.box_size())
          + self.stsc.box_size()
          + self.stsz.box_size()
          + self.stco.as_ref().map_or(0, |b| b.box_size())
          + self.co64.as_ref().map_or(0, |b| b.box_size())
    }

    // Writes the `StblBox` to the provided buffer.
    // The method serializes the box size, box type, and the contents of the `StsdBox`,
    // `SttsBox`, `StscBox`, `StszBox`, and `StcoBox` into the buffer.
    fn write_box(&self, buffer: &mut Vec<u8>) {
        // Write the size of the box in big-endian format.
        buffer.extend_from_slice(&self.box_size().to_be_bytes());
        // Write the box type ("stbl").
        buffer.extend_from_slice(&self.box_type());

        let current_size = buffer.len();
        let stsd_size = self.stsd.box_size() as usize;
        self.stsd.write_box(buffer);
        if buffer.len() != current_size + stsd_size {
            panic!("Error writing StsdBox: expected size {}, got {}", stsd_size, buffer.len() - current_size);
        }
        let current_size = buffer.len();
        let stts_size = self.stts.box_size() as usize;
        self.stts.write_box(buffer);
        if buffer.len() != current_size + stts_size {
            panic!("Error writing SttsBox: expected size {}, got {}", stts_size, buffer.len() - current_size);
        }
        if let Some(ctts) = &self.ctts {
            let current_size = buffer.len();
            let ctts_size = ctts.box_size() as usize;
            ctts.write_box(buffer);
            if buffer.len() != current_size + ctts_size {
                panic!("Error writing CttsBox: expected size {}, got {}", ctts_size, buffer.len() - current_size);
            }
        }
        if let Some(stss) = &self.stss {
            let current_size = buffer.len();
            let stss_size = stss.box_size() as usize;
            stss.write_box(buffer);
            if buffer.len() != current_size + stss_size {
                panic!("Error writing StssBox: expected size {}, got {}", stss_size, buffer.len() - current_size);
            }
        }
        let current_size = buffer.len();
        let stsc_size = self.stsc.box_size() as usize;
        self.stsc.write_box(buffer);
        if buffer.len() != current_size + stsc_size {
            panic!("Error writing StscBox: expected size {}, got {}", stsc_size, buffer.len() - current_size);
        }
        let current_size = buffer.len();
        let stsz_size = self.stsz.box_size() as usize;
        self.stsz.write_box(buffer);
        if buffer.len() != current_size + stsz_size {
            panic!("Error writing StszBox: expected size {}, got {}", stsz_size, buffer.len() - current_size);
        }
        if let Some(stco) = &self.stco {
            let current_size = buffer.len();
            let stco_size = stco.box_size() as usize;
            stco.write_box(buffer);
            if buffer.len() != current_size + stco_size {
                panic!("Error writing StcoBox: expected size {}, got {}", stco_size, buffer.len() - current_size);
            }
        }
        if let Some(co64) = &self.co64 {
            let current_size = buffer.len();
            let co64_size = co64.box_size() as usize;
            co64.write_box(buffer);
            if buffer.len() != current_size + co64_size {
                panic!("Error writing Co64Box: expected size {}, got {}", co64_size, buffer.len() - current_size);
            }
        }
    }

    fn read_box(data: &[u8]) -> Result<(Self, usize), String> {
        let size = u32::from_be_bytes(data[0..4].try_into().unwrap()) as usize;
        if data.len() < size {
            return Err("Incomplete STBL box".into());
        }
        if &data[4..8] != b"stbl" {
            return Err("Not an STBL box".into());
        }

        let mut offset = 8;
        let mut stsd = None;
        let mut stts = None;
        let mut ctts = None;
        let mut stss = None;
        let mut stsc = None;
        let mut stsz = None;
        let mut stco = None;
        let mut co64 = None;

        while offset < size {
            let box_type = &data[offset+4..offset+8];
            let box_size = u32::from_be_bytes(data[offset..offset+4].try_into().unwrap()) as usize;

            match box_type {
                b"stsd" => { let (b, _) = StsdBox::read_box(&data[offset..])?; stsd = Some(b); }
                b"stts" => { let (b, _) = SttsBox::read_box(&data[offset..])?; stts = Some(b); }
                b"ctts" => { let (b, _) = CttsBox::read_box(&data[offset..])?; ctts = Some(b); }
                b"stss" => { let (b, _) = StssBox::read_box(&data[offset..])?; stss = Some(b); }
                b"stsc" => { let (b, _) = StscBox::read_box(&data[offset..])?; stsc = Some(b); }
                b"stsz" => { let (b, _) = StszBox::read_box(&data[offset..])?; stsz = Some(b); }
                b"stco" => { let (b, _) = StcoBox::read_box(&data[offset..])?; stco = Some(b); }
                b"co64" => { let (b, _) = Co64Box::read_box(&data[offset..])?; co64 = Some(b); }
                _ => return Err("Unknown box in STBL".into()),
            }

            offset += box_size;
        }

        // Mandatory checks
        Ok((
            StblBox {
                stsd: stsd.ok_or("Missing STSD box")?,
                stts: stts.ok_or("Missing STTS box")?,
                ctts,
                stss,
                stsc: stsc.ok_or("Missing STSC box")?,
                stsz: stsz.ok_or("Missing STSZ box")?,
                stco,
                co64
            },
            size
        ))
    }
}
