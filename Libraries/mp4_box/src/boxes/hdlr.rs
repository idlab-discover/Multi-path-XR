use crate::format_fourcc;

use super::generic::Mp4Box;

// The `HdlrBox` struct represents a Handler Reference Box in the MP4 file format.
// This box specifies the type of media (e.g., video, audio) and provides a name for the handler.
// It contains the following fields:
// - `handler_type`: A 4-byte array indicating the type of media (e.g., "vide" for video).
// - `name`: A null-terminated string providing a human-readable name for the handler.
#[derive(Clone)]
pub struct HdlrBox {
    pub version: u8,
    pub flags: u32,
    pub handler_type: [u8; 4], // Type of media (e.g., "vide" for video).
    pub name: String,  // Null-terminated string providing the handler name.
}

// Provides a default implementation for the `HdlrBox` struct.
// The default `HdlrBox` has the following values:
// - `handler_type`: "vide" (indicating a video track).
// - `name`: "PointCloudHandler".
impl Default for HdlrBox {
    fn default() -> Self {
        HdlrBox {
            version: 0,
            flags: 0,
            handler_type: *b"vide",   // Video track
            name: "PointCloudHandler".to_string(),
        }
    }
}

impl std::fmt::Debug for HdlrBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HdlrBox")
            .field("box_size", &self.box_size())
            .field("box_type", &format_fourcc(&self.box_type()))
            .field("version", &self.version)
            .field("flags", &format!("0x{:06X}", self.flags))
            .field("handler_type", &self.handler_type)
            .field("name", &self.name)
            .finish()
    }
}


// Implementation of the `Mp4Box` trait for the `HdlrBox` struct.
impl Mp4Box for HdlrBox {
    // Returns the box type as a 4-byte array. For `HdlrBox`, the type is "hdlr".
    fn box_type(&self) -> [u8; 4] { *b"hdlr" }

    // Calculates the size of the `HdlrBox` in bytes.
    // The size includes:
    // - 8 bytes for the header (4 bytes for size and 4 bytes for type).
    // - 4 bytes for the version and flags.
    // - 4 bytes for the `pre_defined` field.
    // - 4 bytes for the `handler_type` field.
    // - 12 bytes for the `reserved` array (3 x 4 bytes).
    // - The length of the `name` field plus 1 byte for the null-terminator.
    fn box_size(&self) -> u32 {
        8 + 4 + 4 + 4 + 12 + (self.name.len() as u32 + 1)  // +1 for null-terminator
    }

    // Writes the `HdlrBox` to the provided buffer.
    // The method serializes the box size, box type, version, flags, `pre_defined`,
    // `handler_type`, `reserved` array, and the `name` field into the buffer.
    fn write_box(&self, buffer: &mut Vec<u8>) {
        // Write the size of the box in big-endian format.
        buffer.extend_from_slice(&self.box_size().to_be_bytes());
        // Write the box type ("hdlr").
        buffer.extend_from_slice(&self.box_type());
        // Write the version (1 byte) and flags (3 bytes).
        buffer.push(self.version);
        buffer.extend_from_slice(&(self.flags & 0x00FFFFFF).to_be_bytes()[1..]);  // 3-byte flags
        
        // Write the `pre_defined` field (4 bytes, set to 0).
        buffer.extend_from_slice(&0u32.to_be_bytes());
        // Write the `handler_type` field (4 bytes).
        buffer.extend_from_slice(&self.handler_type);   // e.g., b"vide"
        // Write the `reserved` array (3 x 4 bytes, all set to 0).
        buffer.extend_from_slice(&[0u32.to_be_bytes(); 3].concat());

        // Write the `name` field as a null-terminated string.
        buffer.extend_from_slice(self.name.as_bytes());
        buffer.push(0);  // Null-terminator for the name
    }

    fn read_box(data: &[u8]) -> Result<(Self, usize), String> {
        if data.len() < 24 {
            return Err("HDLR box too small".into());
        }

        let size = u32::from_be_bytes(data[0..4].try_into().unwrap()) as usize;
        if &data[4..8] != b"hdlr" {
            return Err("Not a HDLR box".into());
        }

        let version = data[8];
        let flags = u32::from_be_bytes([0, data[9], data[10], data[11]]);
        let handler_type = data[16..20].try_into().unwrap();

        let name_start = 24;
        let name_end = data[name_start..size]
            .iter()
            .position(|&b| b == 0)
            .map(|pos| name_start + pos)
            .unwrap_or(size);

        let name = String::from_utf8_lossy(&data[name_start..name_end]).to_string();

        Ok((
            HdlrBox {
                version,
                flags,
                handler_type,
                name,
            },
            size
        ))
    }
}
