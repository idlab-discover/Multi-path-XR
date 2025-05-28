use crate::format_fourcc;

use super::{generic::Mp4Box, mehd::MehdBox, trex::TrexBox};

// The `MvexBox` struct represents a Movie Extends Box in the MP4 file format.
// This box is used in fragmented MP4 files to provide information for movie fragments.
// It contains the following field:
// - `trex_entries`: A vector of `TrexBox` instances, where each `TrexBox` provides default values for track fragments.
//   There is typically one `TrexBox` per track in the movie.
#[derive(Clone)]
pub struct MvexBox { // Movie Extends Box
    pub mehd: Option<MehdBox>,         // Movie Extends Header Box (optional)
    pub trex_entries: Vec<TrexBox>,    // One TrexBox per track
}

// Provides a default implementation for the `MvexBox` struct.
// The default `MvexBox` contains a single `TrexBox` with default values.
impl Default for MvexBox {
    fn default() -> Self {
        MvexBox {
            mehd: None,
            trex_entries: vec![
                TrexBox::default()
            ],
        }
    }
}

impl std::fmt::Debug for MvexBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MvexBox")
            .field("box_size", &self.box_size())
            .field("box_type", &format_fourcc(&self.box_type()))
            .field("mehd", &self.mehd)
            .field("trex_entries", &self.trex_entries)
            .finish()
    }
}

// Implementation of the `Mp4Box` trait for the `MvexBox` struct.
impl Mp4Box for MvexBox {
    // Returns the box type as a 4-byte array. For `MvexBox`, the type is "mvex".
    fn box_type(&self) -> [u8; 4] { *b"mvex" }

    // Calculates the size of the `MvexBox` in bytes.
    // The size includes:
    // - 8 bytes for the header (4 bytes for size and 4 bytes for type).
    // - The size of all `TrexBox` entries in the `trex_entries` vector.
    fn box_size(&self) -> u32 {
        8 
        + self.mehd.as_ref().map_or(0, |m| m.box_size()) 
        + self.trex_entries.iter().map(|trex| trex.box_size()).sum::<u32>()
    }

    // Writes the `MvexBox` to the provided buffer.
    // The method serializes the box size, box type, and the contents of all `TrexBox` entries into the buffer.
    fn write_box(&self, buffer: &mut Vec<u8>) {
        // Write the size of the box in big-endian format.
        buffer.extend_from_slice(&self.box_size().to_be_bytes());
        // Write the box type ("mvex").
        buffer.extend_from_slice(&self.box_type());

        if let Some(mehd_box) = &self.mehd {
            let current_size = buffer.len();
            let mehd_size = mehd_box.box_size() as usize;
            mehd_box.write_box(buffer);
            if buffer.len() != current_size + mehd_size {
                panic!("Error writing MehdBox: expected size {}, got {}", mehd_size, buffer.len() - current_size);
                
            }
        }
        // Write the contents of each `TrexBox` in the `trex_entries` vector.
        for trex in &self.trex_entries {
            let current_size = buffer.len();
            let trex_size = trex.box_size() as usize;
            trex.write_box(buffer);
            if buffer.len() != current_size + trex_size {
                panic!("Error writing TrexBox: expected size {}, got {}", trex_size, buffer.len() - current_size);
            }
        }
    }

    fn read_box(data: &[u8]) -> Result<(Self, usize), String> {
        let size = u32::from_be_bytes(data[0..4].try_into().unwrap()) as usize;
        if data.len() < size {
            return Err("Incomplete MVEX box".into());
        }
        if &data[4..8] != b"mvex" {
            return Err("Not an MVEX box".into());
        }

        let mut offset = 8;
        let mut mehd = None;
        let mut trex_entries = Vec::new();

        while offset < size {
            if offset + 8 > size {
                return Err("Corrupted sub-box in MVEX".into());
            }

            let box_type = &data[offset + 4..offset + 8];
            let sub_box_size = u32::from_be_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;

            if offset + sub_box_size > size {
                return Err("Sub-box size exceeds MVEX bounds".into());
            }

            match box_type {
                b"mehd" => {
                    if mehd.is_some() {
                        return Err("Duplicate MEHD box in MVEX".into());
                    }
                    let (parsed_mehd, _) = MehdBox::read_box(&data[offset..offset + sub_box_size])?;
                    mehd = Some(parsed_mehd);
                },
                b"trex" => {
                    let (parsed_trex, _) = TrexBox::read_box(&data[offset..offset + sub_box_size])?;
                    trex_entries.push(parsed_trex);
                },
                _ => return Err(format!("Unknown box type in MVEX: {:?}", box_type)),
            }

            offset += sub_box_size;
        }

        if trex_entries.is_empty() {
            return Err("MVEX box must contain at least one TREX box".into());
        }

        Ok((MvexBox { mehd, trex_entries }, size))
    }
}
