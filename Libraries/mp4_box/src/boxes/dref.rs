use crate::format_fourcc;

use super::generic::Mp4Box;

// The `DrefBox` struct represents a Data Reference Box in the MP4 file format.
// It contains a list of `DataEntryUrlBox` entries, which specify the data references used in the file.
// Each entry in the list provides information about the location of the data.
#[derive(Clone)]
pub struct DrefBox {
    pub version: u8,
    pub flags: u32,
    pub entries: Vec<DataEntryUrlBox>,  // List of data references
}

// The `DataEntryUrlBox` struct represents a Data Entry URL Box in the MP4 file format.
// It contains a single field `flags` which indicates the nature of the data reference.
// A flag value of `0x000001` indicates that the data is self-contained within the same file.
#[derive(Clone)]
pub struct DataEntryUrlBox {
    pub version: u8,
    pub flags: u32,  // 0x000001 indicates data is in the same file
    pub location: Option<String>,  // Optional location of the data
}

// Provides a default implementation for the `DrefBox` struct.
// The default `DrefBox` contains a single `DataEntryUrlBox` with default values.
impl Default for DrefBox {
    fn default() -> Self {
        DrefBox {
            version: 0,
            flags: 0,
            entries: vec![],
        }
    }
}

// Provides a default implementation for the `DataEntryUrlBox` struct.
// The default `DataEntryUrlBox` has a `flags` value of `0x000001`, indicating self-contained data.
impl Default for DataEntryUrlBox {
    fn default() -> Self {
        DataEntryUrlBox {
            version: 0,
            flags: 0x000001,  // Self-contained data
            location: None,  // No location specified
        }
    }
}

impl std::fmt::Debug for DrefBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DrefBox")
            .field("box_size", &self.box_size())
            .field("box_type", &format_fourcc(&self.box_type()))
            .field("version", &self.version)
            .field("flags", &format!("0x{:06X}", self.flags))
            .field("entries", &self.entries)
            .finish()
    }
}

impl std::fmt::Debug for DataEntryUrlBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut dbg = f.debug_struct("DataEntryUrlBox");
        dbg.field("version", &self.version)
           .field("flags", &format!("0x{:06X}", self.flags));
        if let Some(loc) = &self.location {
            dbg.field("location", loc);
        }
        dbg.finish()
    }
}

// Implementation of the `Mp4Box` trait for the `DrefBox` struct.
impl Mp4Box for DrefBox {
    // Returns the box type as a 4-byte array. For `DrefBox`, the type is "dref".
    fn box_type(&self) -> [u8; 4] { *b"dref" }

    // Calculates the size of the `DrefBox` in bytes.
    // The size includes 8 bytes for the header (4 bytes for size and 4 bytes for type),
    // 4 bytes for the version and flags
    // 4 bytes for the size of all contained `DataEntryUrlBox` entries.
    fn box_size(&self) -> u32 {
        8 + 4 + 4 + self.entries.iter().map(|e| e.box_size()).sum::<u32>()
    }

    // Writes the `DrefBox` to the provided buffer.
    // The method serializes the box size, box type, version, flags, and all contained `DataEntryUrlBox` entries into the buffer.
    fn write_box(&self, buffer: &mut Vec<u8>) {
        // Write the size of the box in big-endian format.
        buffer.extend_from_slice(&self.box_size().to_be_bytes());
        // Write the box type ("dref").
        buffer.extend_from_slice(&self.box_type());
        // Write the version (1 byte) and flags (3 bytes).
        buffer.push(self.version);
        buffer.extend_from_slice(&(self.flags & 0x00FFFFFF).to_be_bytes()[1..]);  // 3-byte flags
        // Write the number of entries in the `entries` list.
        buffer.extend_from_slice(&(self.entries.len() as u32).to_be_bytes());
        // Write each `DataEntryUrlBox` entry to the buffer.
        for entry in &self.entries {
            let current_size = buffer.len();
            let entry_size = entry.box_size() as usize;
            entry.write_box(buffer);
            if buffer.len() != current_size + entry_size {
                panic!("Error writing DataEntryUrlBox: expected size {}, got {}", entry_size, buffer.len() - current_size);
            }
        }
    }

    // Reads a `DrefBox` from the provided data buffer.
    // The method extracts the box size, box type, version, flags, and all contained `DataEntryUrlBox` entries from the data.
    // It returns a tuple containing the `DrefBox` and the size of the box.
    // If the data is incomplete or not a valid `DrefBox`, an error is returned.
    fn read_box(data: &[u8]) -> Result<(Self, usize), String> {
        // Read the size of the box from the first 4 bytes.
        if data.len() < 16 {
            return Err("DREF box too small".into());
        }
        // Read the size of the box from the first 4 bytes.
        let size = u32::from_be_bytes(data[0..4].try_into().unwrap()) as usize;
        // The size must be at least 8 bytes (4 for size and 4 for type).
        if size < 8 || data.len() < size {
            return Err("Incomplete DREF box".into());
        }
        // Check if the box type is "dref".
        // The type is stored in the next 4 bytes.
        if &data[4..8] != b"dref" {
            return Err("Not a DREF box".into());
        }

        // Read how many entries are in the box.
        let version = data[8];
        let flags = u32::from_be_bytes([0, data[9], data[10], data[11]]);
        let entry_count = u32::from_be_bytes(data[12..16].try_into().unwrap());

        let mut entries = Vec::new();
        let mut offset = 16;

        // Read each `DataEntryUrlBox` entry from the data.
        for _ in 0..entry_count {
            // Each entry is expected to be at least 12 bytes (8 for header + 4 for version & flags).
            if offset + 12 > size {
                return Err("Incomplete DataEntryUrlBox".into());
            }

            // Read the type of the entry.
            let entry_size = u32::from_be_bytes(data[offset..offset+4].try_into().unwrap()) as usize;
            let box_type = &data[offset+4..offset+8];
            if box_type != b"url " {
                return Err("Unsupported data entry box".into());
            }

            let version = data[offset+8];
            let flags = u32::from_be_bytes([0, data[offset+9], data[offset+10], data[offset+11]]);
            let location = if entry_size > 12 {
                let loc_bytes = &data[offset+12..offset+entry_size];
                let loc = String::from_utf8(loc_bytes.to_vec()).unwrap_or_default();
                Some(loc.trim_end_matches('\0').to_string())
            } else {
                None
            };

            entries.push(DataEntryUrlBox { version, flags, location });
            offset += entry_size;
        }

        Ok((DrefBox { version, flags, entries }, size))
    }
}

// Implementation of methods for the `DataEntryUrlBox` struct.
impl DataEntryUrlBox {
    // Calculates the size of the `DataEntryUrlBox` in bytes.
    // The size includes 8 bytes for the header (4 bytes for size and 4 bytes for type),
    // and 4 bytes for the version and flags. No payload is included if the flag is `0x000001`.
    fn box_size(&self) -> u32 {
        12  // 8 bytes header + 4 bytes for version & flags, no payload if flag == 1
    }

    // Writes the `DataEntryUrlBox` to the provided buffer.
    // The method serializes the box size, box type, version, and flags into the buffer.
    fn write_box(&self, buffer: &mut Vec<u8>) {
        // Write the size of the box in big-endian format.
        buffer.extend_from_slice(&self.box_size().to_be_bytes());
        // Write the box type ("url ").
        buffer.extend_from_slice(b"url ");
        // Write the version (1 byte).
        buffer.push(0);
        // Write the flags (3 bytes).
        buffer.extend_from_slice(&(self.flags & 0x00FFFFFF).to_be_bytes()[1..]);  // 3-byte flags
    }
}
