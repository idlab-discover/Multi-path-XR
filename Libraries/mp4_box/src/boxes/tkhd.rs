use crate::format_fourcc;

use super::generic::Mp4Box;

// The `TkhdBox` struct represents a Track Header Box in the MP4 file format.
// This box contains metadata about a specific track, such as its creation and modification times,
// track ID, duration, dimensions, and flags indicating its state (e.g., enabled, in movie, in preview).
//
// Fields:
// - `creation_time`: A 32-bit unsigned integer representing the creation time of the track.
// - `modification_time`: A 32-bit unsigned integer representing the last modification time of the track.
// - `track_id`: A 32-bit unsigned integer representing the unique ID of the track.
// - `duration`: A 32-bit unsigned integer representing the duration of the track in timescale units.
// - `width`: A 32-bit unsigned integer in 16.16 fixed-point format representing the width of the track.
// - `height`: A 32-bit unsigned integer in 16.16 fixed-point format representing the height of the track.
// - `flags`: A 32-bit unsigned integer representing the state of the track (e.g., enabled, in movie, in preview).
#[derive(Clone)]
pub struct TkhdBox { // Track Header Box
    pub version: u8,
    pub flags: u32,
    pub creation_time: u64,
    pub modification_time: u64,
    pub track_id: u32,
    pub duration: u64,
    pub layer: u16,
    pub alternate_group: u16,
    pub volume: u16,     // 8.8 fixed-point
    pub width: u32,      // 16.16 fixed-point
    pub height: u32,     // 16.16 fixed-point
}

impl Default for TkhdBox {
    fn default() -> Self {
        TkhdBox {
            version: 0,
            flags: 0x000007,  // enabled, in movie, in preview
            creation_time: 0,
            modification_time: 0,
            track_id: 1,
            duration: 0,
            layer: 0,
            alternate_group: 0,
            volume: 0,  // 0 for video tracks
            width: 640u32 << 16,
            height: 480u32 << 16,
        }
    }
}

impl std::fmt::Debug for TkhdBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TkhdBox")
            .field("box_size", &self.box_size())
            .field("box_type", &format_fourcc(&self.box_type()))
            .field("version", &self.version)
            .field("flags", &format!("0x{:06X}", self.flags))
            .field("track_id", &self.track_id)
            .field("duration", &self.duration)
            .field("width", &format!("{} px", self.width >> 16))
            .field("height", &format!("{} px", self.height >> 16))
            .finish()
    }
}


impl Mp4Box for TkhdBox {
    // Returns the box type as a 4-byte array. For `TkhdBox`, the type is "tkhd".
    fn box_type(&self) -> [u8; 4] { *b"tkhd" }

    // Calculates the size of the `TkhdBox` in bytes.
    // The size includes:
    // - 8 bytes for the header (4 bytes for size and 4 bytes for type).
    // - 4 bytes for the version and flags.
    // - 4|8 bytes for `creation_time`.
    // - 4|8 bytes for `modification_time`.
    // - 4 bytes for `track_id`.
    // - 4 bytes for reserved fields.
    // - 4|8 bytes for `duration`.
    // - 8 bytes for reserved fields.
    // - 2 bytes each for layer, alternate group, and volume.
    // - 2 bytes for reserved field.
    // - 36 bytes for the unity matrix (identity transform).
    // - 4 bytes each for width and height.
    fn box_size(&self) -> u32 {
        let time_fields = if self.version == 1 {
            8 + 8 + 4 + 4 + 8
         } else { 
            4 + 4 + 4 + 4 + 4
          };
        8 + 4 + time_fields + 60  // header + version/flags + times + fixed fields
    }

    // Writes the `TkhdBox` to the provided buffer.
    // The method serializes the box size, box type, version, flags, and all fields into the buffer.
    fn write_box(&self, buffer: &mut Vec<u8>) {
        buffer.extend_from_slice(&self.box_size().to_be_bytes()); // box size (4 bytes)
        buffer.extend_from_slice(&self.box_type()); // box type (4 bytes)

        buffer.push(self.version); // version (1 byte)
        buffer.extend_from_slice(&(self.flags & 0x00FFFFFF).to_be_bytes()[1..]); // flags (3 bytes)

        if self.version == 1 {
            buffer.extend_from_slice(&self.creation_time.to_be_bytes()); // creation_time (8 bytes)
            buffer.extend_from_slice(&self.modification_time.to_be_bytes()); // modification_time (8 bytes)
            buffer.extend_from_slice(&self.track_id.to_be_bytes()); // track_id (4 bytes)
            buffer.extend_from_slice(&0u32.to_be_bytes());  // reserved (4 bytes)
            buffer.extend_from_slice(&self.duration.to_be_bytes()); // duration (8 bytes)
        } else {
            buffer.extend_from_slice(&(self.creation_time as u32).to_be_bytes()); // creation_time (4 bytes)
            buffer.extend_from_slice(&(self.modification_time as u32).to_be_bytes()); // modification_time (4 bytes)
            buffer.extend_from_slice(&self.track_id.to_be_bytes()); // track_id (4 bytes)
            buffer.extend_from_slice(&0u32.to_be_bytes());  // reserved (4 bytes)
            buffer.extend_from_slice(&(self.duration as u32).to_be_bytes()); // duration (4 bytes)
        }

        buffer.extend_from_slice(&0u64.to_be_bytes());  // reserved (8 bytes)
        buffer.extend_from_slice(&self.layer.to_be_bytes()); // layer (2 bytes)
        buffer.extend_from_slice(&self.alternate_group.to_be_bytes()); // alternate group (2 bytes)
        buffer.extend_from_slice(&self.volume.to_be_bytes()); // volume (2 bytes)
        buffer.extend_from_slice(&0u16.to_be_bytes());  // reserved (2 bytes)

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

        buffer.extend_from_slice(&self.width.to_be_bytes()); // width (4 bytes)
        buffer.extend_from_slice(&self.height.to_be_bytes()); // height (4 bytes)
    }

    fn read_box(data: &[u8]) -> Result<(Self, usize), String> {
        let size = u32::from_be_bytes(data[0..4].try_into().unwrap()) as usize;
        if data.len() < size {
            return Err("Incomplete TKHD box".into());
        }
        if &data[4..8] != b"tkhd" {
            return Err("Not a TKHD box".into());
        }

        let version = data[8];
        let flags = u32::from_be_bytes([0, data[9], data[10], data[11]]);
        let mut offset = 12;

        let (creation_time, modification_time, track_id, duration) = if version == 1 {
            let ct = u64::from_be_bytes(data[offset..offset+8].try_into().unwrap());
            let mt = u64::from_be_bytes(data[offset+8..offset+16].try_into().unwrap());
            let tid = u32::from_be_bytes(data[offset+16..offset+20].try_into().unwrap());
            let dur = u64::from_be_bytes(data[offset+24..offset+32].try_into().unwrap());
            offset += 32;
            (ct, mt, tid, dur)
        } else {
            let ct = u32::from_be_bytes(data[offset..offset+4].try_into().unwrap()) as u64;
            let mt = u32::from_be_bytes(data[offset+4..offset+8].try_into().unwrap()) as u64;
            let tid = u32::from_be_bytes(data[offset+8..offset+12].try_into().unwrap());
            let dur = u32::from_be_bytes(data[offset+16..offset+20].try_into().unwrap()) as u64;
            offset += 20;
            (ct, mt, tid, dur)
        };

        offset += 8;  // skip reserved[2]
        let layer = u16::from_be_bytes(data[offset..offset+2].try_into().unwrap());
        let alternate_group = u16::from_be_bytes(data[offset+2..offset+4].try_into().unwrap());
        let volume = u16::from_be_bytes(data[offset+4..offset+6].try_into().unwrap());
        offset += 8;  // skip reserved after volume

        offset += 36;  // skip unity matrix

        let width = u32::from_be_bytes(data[offset..offset+4].try_into().unwrap());
        let height = u32::from_be_bytes(data[offset+4..offset+8].try_into().unwrap());

        Ok((
            TkhdBox {
                version,
                flags,
                creation_time,
                modification_time,
                track_id,
                duration,
                layer,
                alternate_group,
                volume,
                width,
                height,
            },
            size
        ))
    }
}
