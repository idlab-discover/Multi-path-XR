use crate::format_fourcc;

use super::generic::Mp4Box;

// The `MvhdBox` struct represents a Movie Header Box in the MP4 file format.
// This box contains global information about the movie, such as creation and modification times,
// the timescale, duration, playback rate, volume, and the next available track ID.
//
// Fields:
// - `version`: The version of the box. We support both version 0 (32-bit times) and version 1 (64-bit times).
// - `creation_time`: The creation time of the movie, represented as a (32|64)-bit unsigned integer.
// - `modification_time`: The last modification time of the movie, represented as a (32|64)-bit unsigned integer.
// - `timescale`: The timescale of the movie, represented as a 32-bit unsigned integer.
//   This value indicates the number of time units per second.
// - `duration`: The duration of the movie, represented as a (32|64)-bit unsigned integer.
//   This value is expressed in the timescale units.
// - `rate`: The playback rate of the movie, represented as a 16.16 fixed-point number.
//   The default value is `0x00010000`, which corresponds to 1.0 (normal playback speed).
// - `volume`: The playback volume of the movie, represented as an 8.8 fixed-point number.
//   The default value is `0x0100`, which corresponds to 1.0 (full volume).
// - `next_track_id`: The ID of the next available track, represented as a 32-bit unsigned integer.
//   This value is used to assign unique IDs to new tracks.
#[derive(Clone)]
pub struct MvhdBox { // Movie Header Box
    pub version: u8,
    pub creation_time: u64,
    pub modification_time: u64,
    pub timescale: u32,
    pub duration: u64,
    pub rate: u32,      // 16.16 fixed-point (default 0x00010000 for 1.0)
    pub volume: u16,    // 8.8 fixed-point (default 0x0100 for 1.0)
    pub next_track_id: u32,
}

impl Default for MvhdBox {
    fn default() -> Self {
        MvhdBox {
            version: 0,
            creation_time: 0, // Default creation time is 0.
            modification_time: 0, // Default modification time is 0.
            timescale: 30_000, // Default timescale is 30,000 time units per second.
            duration: 0, // Default duration is 0.
            rate: 0x00010000, // Default playback rate is 1.0 (normal speed).
            volume: 0x0100, // Default playback volume is 1.0 (full volume).
            next_track_id: 2, // Default next track ID is 2.
        }
    }
}

impl std::fmt::Debug for MvhdBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MvhdBox")
            .field("box_size", &self.box_size())
            .field("box_type", &format_fourcc(&self.box_type()))
            .field("version", &self.version)
            .field("creation_time", &self.creation_time)
            .field("modification_time", &self.modification_time)
            .field("timescale", &self.timescale)
            .field("duration", &self.duration)
            .field("rate", &self.rate)
            .field("volume", &self.volume)
            .field("next_track_id", &self.next_track_id)
            .finish()
    }
}

impl Mp4Box for MvhdBox {
    fn box_type(&self) -> [u8; 4] { *b"mvhd" }

    fn box_size(&self) -> u32 {
        let time_fields_size = if self.version == 1 { 28 } else { 16 };
        8 + 4 + time_fields_size + 80  // header + version/flags + time fields + rest
    }

    fn write_box(&self, buffer: &mut Vec<u8>) {
        buffer.extend_from_slice(&self.box_size().to_be_bytes());
        buffer.extend_from_slice(&self.box_type());
        buffer.push(self.version);
        buffer.extend_from_slice(&[0; 3]);  // flags = 0

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

        buffer.extend_from_slice(&self.rate.to_be_bytes());
        buffer.extend_from_slice(&self.volume.to_be_bytes());
        buffer.extend_from_slice(&[0; 10]);  // reserved

        // Unity matrix
        buffer.extend_from_slice(&[
            0x00, 0x01, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00,
            0x00, 0x01, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x40, 0x00
        ]);

        buffer.extend_from_slice(&[0; 24]);  // pre_defined
        buffer.extend_from_slice(&self.next_track_id.to_be_bytes());
    }

    fn read_box(data: &[u8]) -> Result<(Self, usize), String> {
        let size = u32::from_be_bytes(data[0..4].try_into().unwrap()) as usize;
        if data.len() < size {
            return Err("Incomplete MVHD box".into());
        }
        if &data[4..8] != b"mvhd" {
            return Err("Not an MVHD box".into());
        }

        let version = data[8];
        let mut offset = 12;

        let (creation_time, modification_time, timescale, duration) = if version == 1 {
            let creation = u64::from_be_bytes(data[offset..offset+8].try_into().unwrap());
            offset += 8;
            let modification = u64::from_be_bytes(data[offset..offset+8].try_into().unwrap());
            offset += 8;
            let timescale = u32::from_be_bytes(data[offset..offset+4].try_into().unwrap());
            offset += 4;
            let duration = u64::from_be_bytes(data[offset..offset+8].try_into().unwrap());
            offset += 8;
            (creation, modification, timescale, duration)
        } else if version == 0 {
            let creation = u32::from_be_bytes(data[offset..offset+4].try_into().unwrap()) as u64;
            offset += 4;
            let modification = u32::from_be_bytes(data[offset..offset+4].try_into().unwrap()) as u64;
            offset += 4;
            let timescale = u32::from_be_bytes(data[offset..offset+4].try_into().unwrap());
            offset += 4;
            let duration = u32::from_be_bytes(data[offset..offset+4].try_into().unwrap()) as u64;
            offset += 4;
            (creation, modification, timescale, duration)
        } else {
            return Err("Unsupported MVHD version".into());
        };

        let rate = u32::from_be_bytes(data[offset..offset+4].try_into().unwrap());
        offset += 4;

        let volume = u16::from_be_bytes(data[offset..offset+2].try_into().unwrap());
        offset += 2;

        // Skip reserved (10 bytes) + matrix (36 bytes) + pre_defined (24 bytes)
        offset += 10 + 36 + 24;

        let next_track_id = u32::from_be_bytes(data[offset..offset+4].try_into().unwrap());

        Ok((
            MvhdBox {
                version,
                creation_time,
                modification_time,
                timescale,
                duration,
                rate,
                volume,
                next_track_id,
            },
            size
        ))
    }
}
