use crate::{format_capped_bytes, format_fourcc};

use super::generic::Mp4Box;

// The `StsdBox` struct represents a Sample Description Box in the MP4 file format.
// This box contains a table of sample descriptions, which describe the format and properties of the media samples.
// Each entry in the table corresponds to a specific type of media sample, such as video or audio.
//
// Fields:
// - `entries`: A vector of `VisualSampleEntry` instances, where each entry describes a specific type of media sample.
//   Typically, there is only one entry in the vector.
#[derive(Clone)]
pub struct StsdBox { // Sample Description Box
    pub version: u8,
    pub flags: u32,
    pub entries: Vec<VisualSampleEntry>,  // Typically 1 entry
}

// The `VisualSampleEntry` struct represents a single entry in the Sample Description Box.
// This entry describes the format and properties of a visual media sample, such as a video frame.
//
// Fields:
// - `data_format`: A 4-byte array indicating the format of the media sample (e.g., `b"pcvc"` for Point Cloud Video Codec).
// - `width`: The width of the visual sample in pixels.
// - `height`: The height of the visual sample in pixels.
// - `compressor_name`: A string (up to 31 bytes) specifying the name of the compressor used for the sample.
// - `codec_config`: An optional vector of bytes containing additional codec configuration data (e.g., `avcC` for H.264).
#[derive(Clone)]
pub struct VisualSampleEntry {
    pub data_format: [u8; 4],  // e.g., b"pcvc"
    pub width: u16,
    pub height: u16,
    pub compressor_name: String,  // Up to 31 bytes
    pub codec_config: Option<Vec<u8>>,  // Optional extra box (like avcC for H264)
}

impl Default for StsdBox {
    fn default() -> Self {
        StsdBox {
            version: 0,
            flags: 0,
            entries: vec![
                VisualSampleEntry::default()
            ],
        }
    }
}

impl Default for VisualSampleEntry {
    fn default() -> Self {
        VisualSampleEntry {
            data_format: *b"pcvc",
            width: 640,
            height: 480,
            compressor_name: "PointCloudCodec".to_string(),
            codec_config: None,
        }
    }
}

impl std::fmt::Debug for StsdBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StsdBox")
            .field("box_size", &self.box_size())
            .field("box_type", &format_fourcc(&self.box_type()))
            .field("version", &self.version)
            .field("flags", &self.flags)
            .field("descriptions", &self.entries)
            .finish()
    }
}

impl std::fmt::Debug for VisualSampleEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VisualSampleEntry")
            .field("data_format", &format_fourcc(&self.data_format))
            .field("width", &self.width)
            .field("height", &self.height)
            .field("compressor_name", &self.compressor_name)
            .field("codec_config", &self.codec_config.as_ref().map(|c| format_capped_bytes(c)))
            .finish()
    }
}

impl Mp4Box for StsdBox {
    // Returns the box type as a 4-byte array. For `StsdBox`, the type is "stsd".
    fn box_type(&self) -> [u8; 4] { *b"stsd" }

    // Calculates the size of the `StsdBox` in bytes.
    // The size includes:
    // - 16 bytes for the header (4 bytes for size, 4 bytes for type, 4 bytes for version/flags, and 4 bytes for entry count).
    // - The size of all `VisualSampleEntry` instances in the `entries` vector.
    fn box_size(&self) -> u32 {
        16 + self.entries.iter().map(|e| e.box_size()).sum::<u32>()
    }

    // Writes the `StsdBox` to the provided buffer.
    // The method serializes the box size, box type, version, flags, entry count, and all `VisualSampleEntry` instances into the buffer.
    fn write_box(&self, buffer: &mut Vec<u8>) {
        buffer.extend_from_slice(&self.box_size().to_be_bytes());
        buffer.extend_from_slice(&self.box_type());
        buffer.push(self.version);
        buffer.extend_from_slice(&self.flags.to_be_bytes()[1..4]);  // flags (24 bits)
        buffer.extend_from_slice(&(self.entries.len() as u32).to_be_bytes());
        for entry in &self.entries {
            let current_size = buffer.len();
            let entry_size = entry.box_size() as usize;
            entry.write_box(buffer);
            if buffer.len() != current_size + entry_size {
                panic!("Error writing VisualSampleEntry: expected size {}, got {}", entry_size, buffer.len() - current_size);
            }
        }
    }

    fn read_box(data: &[u8]) -> Result<(Self, usize), String> {
        let size = u32::from_be_bytes(data[0..4].try_into().unwrap()) as usize;
        if data.len() < size {
            return Err("Incomplete STSD box".into());
        }
        if &data[4..8] != b"stsd" {
            return Err("Not an STSD box".into());
        }
    
        let version = data[8];
        let mut flag_bytes = [0u8; 4];
        flag_bytes[1..4].copy_from_slice(&data[9..12]);
        let flags = u32::from_be_bytes(flag_bytes);
    
        let entry_count = u32::from_be_bytes(data[12..16].try_into().unwrap());
        let mut entries = Vec::new();
        let mut offset = 16;
    
        for _ in 0..entry_count {
            if offset + 8 > size {
                return Err("Incomplete VisualSampleEntry header".into());
            }
    
            let box_size = u32::from_be_bytes(data[offset..offset+4].try_into().unwrap()) as usize;
            let data_format = data[offset+4..offset+8].try_into().unwrap();
            
            if offset + box_size > size {
                return Err("VisualSampleEntry box extends beyond parent box".into());
            }
    
            let width = u16::from_be_bytes(data[offset+32..offset+34].try_into().unwrap());
            let height = u16::from_be_bytes(data[offset+34..offset+36].try_into().unwrap());
    
            let name_len = data[offset+50] as usize;
            let compressor_name_end = std::cmp::min(offset+51+name_len, offset+84); // clamp to avoid reading beyond base structure
            let compressor_name = String::from_utf8_lossy(
                &data[offset+51..compressor_name_end]
            ).to_string();
    
            let mut codec_config = None;
            let mut sub_offset = offset + 86; // after the base VisualSampleEntry structure
            while sub_offset + 8 <= offset + box_size {
                let sub_box_size = u32::from_be_bytes(data[sub_offset..sub_offset+4].try_into().unwrap()) as usize;
                let sub_box_type = &data[sub_offset+4..sub_offset+8];
    
                if sub_box_type == b"pccc" || sub_box_type == b"avcC" || sub_box_type == b"esds" {
                    codec_config = Some(data[sub_offset+8..sub_offset+sub_box_size].to_vec());
                }
                sub_offset += sub_box_size;
            }
    
            entries.push(VisualSampleEntry {
                data_format,
                width,
                height,
                compressor_name,
                codec_config,
            });
    
            offset += box_size;
        }
    
        Ok((StsdBox { version, flags, entries }, size))
    }
}

// Implementation of methods for the `VisualSampleEntry` struct.
impl VisualSampleEntry {
    // Calculates the size of the `VisualSampleEntry` in bytes.
    // The size includes:
    // - 8 bytes for the header (4 bytes for size and 4 bytes for data format).
    // - 6 bytes for reserved fields.
    // - 2 bytes for the data reference index.
    // - 16 bytes for pre-defined and reserved fields.
    // - 2 bytes for width.
    // - 2 bytes for height.
    // - 12 bytes for horizontal and vertical resolution, reserved field, and frame count.
    // - 32 bytes for the compressor name (Pascal string, up to 31 bytes plus 1 byte for length).
    // - 4 bytes for depth and pre-defined fields.
    // - The size of the optional codec configuration data, if present.
    fn box_size(&self) -> u32 {
        let base_size = 86;
        let config_len = self.codec_config.as_ref().map_or(0, |c| c.len() as u32 + 8);
        base_size + config_len
    }

    // Writes the `VisualSampleEntry` to the provided buffer.
    // The method serializes the entry's fields and optional codec configuration data into the buffer.
    fn write_box(&self, buffer: &mut Vec<u8>) {
        buffer.extend_from_slice(&self.box_size().to_be_bytes());
        buffer.extend_from_slice(&self.data_format);
        buffer.extend_from_slice(&[0; 6]);  // reserved
        buffer.extend_from_slice(&1u16.to_be_bytes());  // data_reference_index
        buffer.extend_from_slice(&[0; 16]);  // pre_defined + reserved
        buffer.extend_from_slice(&self.width.to_be_bytes());
        buffer.extend_from_slice(&self.height.to_be_bytes());
        buffer.extend_from_slice(&0x00480000u32.to_be_bytes());  // horizresolution (72 dpi)
        buffer.extend_from_slice(&0x00480000u32.to_be_bytes());  // vertresolution
        buffer.extend_from_slice(&0u32.to_be_bytes());           // reserved
        buffer.extend_from_slice(&1u16.to_be_bytes());           // frame_count

        // Compressor name (Pascal string, max 31 bytes)
        let mut name_bytes = vec![self.compressor_name.len() as u8];
        name_bytes.extend_from_slice(self.compressor_name.as_bytes());
        name_bytes.resize(32, 0);
        buffer.extend_from_slice(&name_bytes);

        buffer.extend_from_slice(&0x0018u16.to_be_bytes());  // depth = 24
        buffer.extend_from_slice(&0xffffu16.to_be_bytes());  // pre_defined

        // Optional codec config box
        if let Some(config) = &self.codec_config {
            buffer.extend_from_slice(&(config.len() as u32 + 8).to_be_bytes());
            buffer.extend_from_slice(b"pccc");  // Example custom config box type
            buffer.extend_from_slice(config);
        }
    }
}
