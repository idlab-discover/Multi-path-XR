use crate::format_fourcc;

use super::generic::Mp4Box;

// The `FtypBox` struct represents a File Type Box in the MP4 file format.
// This box specifies the file type and compatibility information for the MP4 file.
// It contains the following fields:
// - `major_brand`: A 4-byte array indicating the major brand of the file.
// - `minor_version`: A 32-bit unsigned integer indicating the minor version of the major brand.
// - `compatible_brands`: A vector of 4-byte arrays indicating other compatible brands.
#[derive(Clone)]
pub struct FtypBox {
    pub major_brand: [u8; 4], // Major brand of the file.
    pub minor_version: u32,   // Minor version of the major brand.
    pub compatible_brands: Vec<[u8; 4]>, // List of compatible brands.
}

// Provides a default implementation for the `FtypBox` struct.
// The default `FtypBox` has the following values:
// - `major_brand`: "isom" (ISO Base Media File Format).
// - `minor_version`: 0.
// - `compatible_brands`: ["isom", "iso6", "dash"].
impl Default for FtypBox {
    fn default() -> Self {
        FtypBox {
            major_brand: *b"isom",
            minor_version: 0,
            compatible_brands: vec![
                *b"isom",
                *b"iso6",
                *b"dash",
            ],
        }
    }
}

impl std::fmt::Debug for FtypBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FtypBox")
            .field("box_size", &self.box_size())
            .field("box_type", &format_fourcc(&self.box_type()))
            .field("major_brand", &format_fourcc(&self.major_brand))
            .field("minor_version", &self.minor_version)
            .field("compatible_brands", 
                &self.compatible_brands.iter()
                    .map(format_fourcc)
                    .collect::<Vec<_>>()
            )
            .finish()
    }
}

// Implementation of the `Mp4Box` trait for the `FtypBox` struct.
impl Mp4Box for FtypBox {
    // Returns the box type as a 4-byte array. For `FtypBox`, the type is "ftyp".
    fn box_type(&self) -> [u8; 4] { *b"ftyp" }

    // Calculates the size of the `FtypBox` in bytes.
    // The size includes:
    // - 8 bytes for the header (4 bytes for size and 4 bytes for type).
    // - 4 bytes for the `major_brand`.
    // - 4 bytes for the `minor_version`.
    // - 4 bytes for each entry in the `compatible_brands` vector.
    fn box_size(&self) -> u32 {
        8 + 4 + 4 + (4 * self.compatible_brands.len() as u32)
    }

    // Writes the `FtypBox` to the provided buffer.
    // The method serializes the box size, box type, `major_brand`, `minor_version`,
    // and all entries in the `compatible_brands` vector into the buffer.
    fn write_box(&self, buffer: &mut Vec<u8>) {
        // Write the size of the box in big-endian format.
        buffer.extend_from_slice(&self.box_size().to_be_bytes());
        // Write the box type ("ftyp").
        buffer.extend_from_slice(&self.box_type());
        // Write the `major_brand`.
        buffer.extend_from_slice(&self.major_brand);
        // Write the `minor_version` in big-endian format.
        buffer.extend_from_slice(&self.minor_version.to_be_bytes());
        // Write each compatible brand in the `compatible_brands` vector.
        for brand in &self.compatible_brands {
            buffer.extend_from_slice(brand);
        }
    }

    // Reads a `FtypBox` from the provided data buffer.
    // The method extracts the box size, box type, `major_brand`, `minor_version`,
    // and all entries in the `compatible_brands` vector from the data.
    // It returns a tuple containing the `FtypBox` and the size of the box.
    // If the data is not sufficient or the box type is incorrect, an error is returned.
    fn read_box(data: &[u8]) -> Result<(Self, usize), String> {
        // Read the size of the box from the first 4 bytes.
        if data.len() < 16 {
            return Err("FTYP box too small".into());
        }

        // Check if the data length is sufficient for the box size.
        let size = u32::from_be_bytes(data[0..4].try_into().unwrap()) as usize;
        // The size must be at least 8 bytes (4 for size and 4 for type).
        if size < 8 || data.len() < size {
            return Err("Incomplete FTYP box".into());
        }
        let box_type = &data[4..8];
        if box_type != b"ftyp" {
            return Err("Not an FTYP box".into());
        }

        let major_brand = data[8..12].try_into().unwrap();
        let minor_version = u32::from_be_bytes(data[12..16].try_into().unwrap());

        let mut compatible_brands = Vec::new();
        let mut offset = 16;
        while offset + 4 <= size {
            compatible_brands.push(data[offset..offset+4].try_into().unwrap());
            offset += 4;
        }

        Ok((
            FtypBox {
                major_brand,
                minor_version,
                compatible_brands,
            },
            size
        ))
    }
}
