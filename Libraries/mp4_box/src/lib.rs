//! # MP4 File Format Overview
//! 
//! The MP4 file format is a container format designed to store multimedia data such as video,
//! audio, subtitles, and metadata. It is widely used for streaming and storing media due to its
//! flexibility, efficiency, and compatibility.
//! 
//! ## Structure of an MP4 File
//! An MP4 file is composed of a hierarchical structure of **boxes** (also called atoms). Each box
//! serves a specific purpose, such as storing metadata, media data, or structural information.
//! 
//! ### Key Characteristics of MP4 Boxes
//! - **Box Header**: Each box begins with a header that specifies its size and type.
//! - **Box Type**: A 4-character code (e.g., `ftyp`, `moov`, `mdat`) that identifies the purpose of the box.
//! - **Box Payload**: The content of the box, which can include other nested boxes or raw data.
//! 
//! ### Common MP4 Boxes
//! 1. **File Type Box (`ftyp`)**:
//!    - Specifies the file type and compatibility information.
//!    - Located at the beginning of the file.
//! 
//! 2. **Movie Box (`moov`)**:
//!    - Contains metadata for the entire movie, such as track information and timing.
//!    - Includes sub-boxes like `mvhd` (Movie Header Box) and `trak` (Track Box).
//! 
//! 3. **Media Data Box (`mdat`)**:
//!    - Stores the raw media data (e.g., video frames, audio samples).
//!    - Typically the largest box in the file.
//! 
//! 4. **Free Space Box (`free`)**:
//!    - Used for padding or reserving space for future edits.
//! 
//! 5. **Movie Fragment Boxes (`moof` and `mfra`)**:
//!    - Used in fragmented MP4 files to enable streaming and progressive download.
//! 
//! ## MP4 File Workflow
//! 1. **Initialization**:
//!    - The `ftyp` box is read to determine compatibility.
//!    - The `moov` box is parsed to extract metadata and track information.
//! 
//! 2. **Playback**:
//!    - The `mdat` box is accessed to retrieve raw media data.
//!    - Timing and synchronization are managed using information from the `moov` box.
//! 
//! 3. **Streaming**:
//!    - Fragmented MP4 files use `moof` and `mdat` boxes to deliver media in chunks.
//!    - This approach reduces latency and enables adaptive bitrate streaming.
//! 
//! ## Advantages of the MP4 Format
//! - **Compatibility**: Supported by most devices and platforms.
//! - **Efficiency**: Optimized for storage and streaming.
//! - **Extensibility**: Allows for custom boxes and extensions.
//! 
//! ## Implementation in This Library
//! This library provides modules and structures to parse, manipulate, and generate MP4 files.
//! - The `boxes` module defines various MP4 box types and their functionality.
//! - The `mp4streamconfig` module handles configuration and streaming-related operations.
//! 

pub mod boxes;
pub mod writer;
pub mod reader;

pub fn format_fourcc(fourcc: &[u8; 4]) -> String {
    std::str::from_utf8(fourcc).unwrap_or("????").to_string()
}

pub fn format_capped_bytes(data: &[u8]) -> String {
    let capped = &data[..data.len().min(8)];
    if data.len() > 8 {
        format!("{:?} ...", capped)
    } else {
        format!("{:?}", capped)
    }
}

pub fn read_u32_be(data: &[u8], offset: usize) -> Result<u32, String> {
    data.get(offset..offset + 4)
        .ok_or("Out of bounds while reading u32".into())
        .map(|bytes| u32::from_be_bytes(bytes.try_into().unwrap()))
}

pub fn read_version_and_flags(data: &[u8]) -> (u8, u32) {
    let version = data[0];
    let flags = ((data[1] as u32) << 16) | ((data[2] as u32) << 8) | data[3] as u32;
    (version, flags)
}

pub fn write_version_and_flags(buffer: &mut Vec<u8>, version: u8, flags: u32) {
    buffer.push(version);
    buffer.push(((flags >> 16) & 0xFF) as u8);
    buffer.push(((flags >> 8) & 0xFF) as u8);
    buffer.push((flags & 0xFF) as u8);
}