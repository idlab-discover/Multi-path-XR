use crate::format_fourcc;

use super::generic::Mp4Box;

// The `MdhdBox` struct represents a Media Header Box in the MP4 file format.
// This box contains metadata about the media, such as creation and modification times,
// the timescale, duration, and language of the media.
//
// Fields:
// - `creation_time`: The creation time of the media, represented as a 32-bit unsigned integer.
// - `modification_time`: The last modification time of the media, represented as a 32-bit unsigned integer.
// - `timescale`: The timescale of the media, represented as a 32-bit unsigned integer.
//   This value indicates the number of time units per second.
// - `duration`: The duration of the media, represented as a 32-bit unsigned integer.
//   This value is expressed in the timescale units.
// - `language`: The language of the media, represented as an ISO 639-2/T language code (e.g., "und").
#[derive(Clone)]
pub struct MdhdBox { // Media Header Box
    pub version: u8,
    pub flags: u32,
    pub creation_time: u64,
    pub modification_time: u64,
    pub timescale: u32,
    pub duration: u64,
    pub language: String,  // ISO 639-2/T language code, e.g., "und"
}

// Provides a default implementation for the `MdhdBox` struct.
// The default `MdhdBox` has the following values:
// - `creation_time`: 0.
// - `modification_time`: 0.
// - `timescale`: 30,000 (indicating 30,000 time units per second).
// - `duration`: 0.
// - `language`: "und" (undefined language).
impl Default for MdhdBox {
    fn default() -> Self {
        MdhdBox {
            version: 0,
            flags: 0,
            creation_time: 0,
            modification_time: 0,
            timescale: 30_000,
            duration: 0,
            language: "und".to_string(),  // ISO 639-2/T language code
        }
    }
}

impl std::fmt::Debug for MdhdBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MdhdBox")
            .field("box_size", &self.box_size())
            .field("box_type", &format_fourcc(&self.box_type()))
            .field("version", &self.version)
            .field("flags", &format!("0x{:06X}", self.flags))
            .field("creation_time", &self.creation_time)
            .field("modification_time", &self.modification_time)
            .field("timescale", &self.timescale)
            .field("duration", &self.duration)
            .field("language", &self.language)
            .finish()
    }
}

// Implementation of the `Mp4Box` trait for the `MdhdBox` struct.
impl Mp4Box for MdhdBox {
    // Returns the box type as a 4-byte array. For `MdhdBox`, the type is "mdhd".
    fn box_type(&self) -> [u8; 4] { *b"mdhd" }

    // Calculates the size of the `MdhdBox` in bytes.
    // The size includes:
    // - 8 bytes for the header (4 bytes for size and 4 bytes for type).
    // - 4 bytes for the version and flags.
    // - 4 bytes each for `creation_time`, `modification_time`, `timescale`, and `duration`.
    // - 2 bytes for the packed language field.
    // - 2 bytes for the `pre_defined` field.
    fn box_size(&self) -> u32 {
        let base = 8 + 4;  // header + version/flags
        let variable = if self.version == 1 { 28 } else { 16 };
        base + variable + 4  // + language (2) + pre_defined (2)
    }

    // Writes the `MdhdBox` to the provided buffer.
    // The method serializes the box size, box type, version, flags, `creation_time`,
    // `modification_time`, `timescale`, `duration`, language, and `pre_defined` fields into the buffer.
    fn write_box(&self, buffer: &mut Vec<u8>) {
        buffer.extend_from_slice(&self.box_size().to_be_bytes());
        buffer.extend_from_slice(&self.box_type());
        buffer.push(self.version);
        buffer.extend_from_slice(&(self.flags & 0x00FFFFFF).to_be_bytes()[1..]);

        if self.version == 1 {
            buffer.extend_from_slice(&self.creation_time.to_be_bytes());
            buffer.extend_from_slice(&self.modification_time.to_be_bytes());
            buffer.extend_from_slice(&self.timescale.to_be_bytes());
            buffer.extend_from_slice(&self.duration.to_be_bytes());
        } else {
            buffer.extend_from_slice(&(self.creation_time as u32).to_be_bytes());
            buffer.extend_from_slice(&(self.modification_time as u32).to_be_bytes());
            buffer.extend_from_slice(&self.timescale.to_be_bytes());
            buffer.extend_from_slice(&(self.duration as u32).to_be_bytes());
        }

        // Language field: 15 bits packed (5 bits per char)
        let lang_code = encode_language(&self.language);
        buffer.extend_from_slice(&lang_code.to_be_bytes());

        buffer.extend_from_slice(&0u16.to_be_bytes());  // pre_defined
    }

    fn read_box(data: &[u8]) -> Result<(Self, usize), String> {
        if data.len() < 32 {
            return Err("MDHD box too small".into());
        }

        let size = u32::from_be_bytes(data[0..4].try_into().unwrap()) as usize;
        if &data[4..8] != b"mdhd" {
            return Err("Not an MDHD box".into());
        }

        let version = data[8];
        let flags = u32::from_be_bytes([0, data[9], data[10], data[11]]);

        let (creation_time, modification_time, timescale, duration, lang_offset) = if version == 1 {
            if size < 44 { return Err("MDHD v1 box too small".into()); }
            (
                u64::from_be_bytes(data[12..20].try_into().unwrap()),
                u64::from_be_bytes(data[20..28].try_into().unwrap()),
                u32::from_be_bytes(data[28..32].try_into().unwrap()),
                u64::from_be_bytes(data[32..40].try_into().unwrap()),
                40
            )
        } else if version == 0 {
            if size < 32 { return Err("MDHD v0 box too small".into()); }
            (
                u32::from_be_bytes(data[12..16].try_into().unwrap()) as u64,
                u32::from_be_bytes(data[16..20].try_into().unwrap()) as u64,
                u32::from_be_bytes(data[20..24].try_into().unwrap()),
                u32::from_be_bytes(data[24..28].try_into().unwrap()) as u64,
                28
            )
        } else {
            return Err("Unsupported MDHD version".into());
        };

        let lang_code = u16::from_be_bytes(data[lang_offset..lang_offset+2].try_into().unwrap());
        let language = decode_language(lang_code);

        Ok((
            MdhdBox {
                version,
                flags,
                creation_time,
                modification_time,
                timescale,
                duration,
                language,
            },
            size
        ))
    }
}

/// Helper function to encode ISO 639-2/T language code to 15-bit packed format.
///
/// This function takes a 3-character ISO 639-2/T language code and encodes it
/// into a 15-bit packed format, as specified in the MP4 file format.
///
/// # Arguments
/// - `lang`: A string slice representing the ISO 639-2/T language code.
///
/// # Returns
/// A 16-bit unsigned integer representing the packed language code.
///
/// # Panics
/// This function will panic if the input string is not exactly 3 characters long.
fn encode_language(lang: &str) -> u16 {
    let bytes = lang.as_bytes();
    if bytes.len() != 3 {
        panic!("Language code must be exactly 3 characters");
    }
    (((bytes[0] - 0x60) as u16) << 10) |
    (((bytes[1] - 0x60) as u16) << 5)  |
    ((bytes[2] - 0x60) as u16)
}


fn decode_language(code: u16) -> String {
    let mut lang = String::new();
    lang.push((((code >> 10) & 0x1F) + 0x60) as u8 as char);
    lang.push((((code >> 5) & 0x1F) + 0x60) as u8 as char);
    lang.push(((code       & 0x1F) + 0x60) as u8 as char);
    lang
}
