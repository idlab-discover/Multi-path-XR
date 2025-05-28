use crate::format_fourcc;

use super::generic::Mp4Box;

// The `TfhdBox` struct represents a Track Fragment Header Box in the MP4 file format.
// This box provides information about a track fragment, including the track ID and optional flags.
// The flags field allows for flexibility in specifying additional properties of the track fragment.
//
// Fields:
// - `track_id`: A 32-bit unsigned integer representing the ID of the track associated with this fragment.
// - `flags`: A 32-bit unsigned integer containing optional flags that specify additional properties of the track fragment.
//   The flags field is designed to allow for future expansion and customization.
#[derive(Clone)]
pub struct TfhdBox { // Track Fragment Header Box
    pub version: u8,       // Version (always 0)
    pub flags: u32,        // 24-bit flags
    pub track_id: u32,     // Track ID

    // Optional fields based on flags
    pub base_data_offset: Option<u64>,
    pub sample_description_index: Option<u32>,
    pub default_sample_duration: Option<u32>,
    pub default_sample_size: Option<u32>,
    pub default_sample_flags: Option<u32>,
}

// Provides a default implementation for the `TfhdBox` struct.
// The default `TfhdBox` has the following values:
// - `track_id`: 1 (indicating the first track).
// - `flags`: 0 (indicating no additional properties).
impl Default for TfhdBox {
    fn default() -> Self {
        TfhdBox {
            version: 0,
            flags: 0,
            track_id: 1,
            base_data_offset: None,
            sample_description_index: None,
            default_sample_duration: None,
            default_sample_size: None,
            default_sample_flags: None,
        }
    }
}

impl std::fmt::Debug for TfhdBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TfhdBox")
            .field("box_size", &self.box_size())
            .field("box_type", &format_fourcc(&self.box_type()))
            .field("version", &self.version)
            .field("flags", &format!("0x{:06X}", self.flags))
            .field("track_id", &self.track_id)
            .field("base_data_offset", &self.base_data_offset)
            .field("sample_description_index", &self.sample_description_index)
            .field("default_sample_duration", &self.default_sample_duration)
            .field("default_sample_size", &self.default_sample_size)
            .field("default_sample_flags", &self.default_sample_flags)
            .finish()
    }
}


// Implementation of the `Mp4Box` trait for the `TfhdBox` struct.
impl Mp4Box for TfhdBox {
    // Returns the box type as a 4-byte array. For `TfhdBox`, the type is "tfhd".
    fn box_type(&self) -> [u8; 4] { *b"tfhd" }

    // Calculates the size of the `TfhdBox` in bytes.
    // The size includes:
    // - 8 bytes for the header (4 bytes for size and 4 bytes for type).
    // - 4 bytes for the version and flags.
    // - 4 bytes for the `track_id` field.
    fn box_size(&self) -> u32 {
        let mut size = 8 + 4 + 4; // header + version/flags + track_id
        if self.flags & 0x000001 != 0 { size += 8; }  // base_data_offset
        if self.flags & 0x000002 != 0 { size += 4; }  // sample_description_index
        if self.flags & 0x000008 != 0 { size += 4; }  // default_sample_duration
        if self.flags & 0x000010 != 0 { size += 4; }  // default_sample_size
        if self.flags & 0x000020 != 0 { size += 4; }  // default_sample_flags
        size
    }

    // Writes the `TfhdBox` to the provided buffer.
    // The method serializes the box size, box type, version, flags, and `track_id` into the buffer.
    fn write_box(&self, buffer: &mut Vec<u8>) {
        buffer.extend_from_slice(&self.box_size().to_be_bytes());
        buffer.extend_from_slice(&self.box_type());
        buffer.push(self.version);
        buffer.extend_from_slice(&(self.flags & 0x00FFFFFF).to_be_bytes()[1..]);
        buffer.extend_from_slice(&self.track_id.to_be_bytes());

        if self.flags & 0x000001 != 0 {
            buffer.extend_from_slice(&self.base_data_offset.unwrap_or(0).to_be_bytes());
        }
        if self.flags & 0x000002 != 0 {
            buffer.extend_from_slice(&self.sample_description_index.unwrap_or(1).to_be_bytes());
        }
        if self.flags & 0x000008 != 0 {
            buffer.extend_from_slice(&self.default_sample_duration.unwrap_or(0).to_be_bytes());
        }
        if self.flags & 0x000010 != 0 {
            buffer.extend_from_slice(&self.default_sample_size.unwrap_or(0).to_be_bytes());
        }
        if self.flags & 0x000020 != 0 {
            buffer.extend_from_slice(&self.default_sample_flags.unwrap_or(0).to_be_bytes());
        }
    }

    fn read_box(data: &[u8]) -> Result<(Self, usize), String> {
        let size = u32::from_be_bytes(data[0..4].try_into().unwrap()) as usize;
        if data.len() < size {
            return Err("Incomplete TFHD box".into());
        }
        if &data[4..8] != b"tfhd" {
            return Err("Not a TFHD box".into());
        }

        let version = data[8];
        let flags = u32::from_be_bytes([0, data[9], data[10], data[11]]);
        let mut offset = 12;

        let track_id = u32::from_be_bytes(data[offset..offset+4].try_into().unwrap());
        offset += 4;

        let base_data_offset = if flags & 0x000001 != 0 {
            let val = u64::from_be_bytes(data[offset..offset+8].try_into().unwrap());
            offset += 8;
            Some(val)
        } else { None };

        let sample_description_index = if flags & 0x000002 != 0 {
            let val = u32::from_be_bytes(data[offset..offset+4].try_into().unwrap());
            offset += 4;
            Some(val)
        } else { None };

        let default_sample_duration = if flags & 0x000008 != 0 {
            let val = u32::from_be_bytes(data[offset..offset+4].try_into().unwrap());
            offset += 4;
            Some(val)
        } else { None };

        let default_sample_size = if flags & 0x000010 != 0 {
            let val = u32::from_be_bytes(data[offset..offset+4].try_into().unwrap());
            offset += 4;
            Some(val)
        } else { None };

        let default_sample_flags = if flags & 0x000020 != 0 {
            let val = u32::from_be_bytes(data[offset..offset+4].try_into().unwrap());
            //offset += 4;
            Some(val)
        } else { None };

        Ok((
            TfhdBox {
                version,
                flags,
                track_id,
                base_data_offset,
                sample_description_index,
                default_sample_duration,
                default_sample_size,
                default_sample_flags,
            },
            size
        ))
    }
}
