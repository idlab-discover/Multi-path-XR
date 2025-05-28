use crate::format_fourcc;

use super::generic::Mp4Box;

/// The `VmhdBox` struct represents a Video Media Header Box (`vmhd`) in the MP4 file format.
/// This box provides video-specific rendering information such as graphics mode and optional color hints.
///
/// Fields:
/// - `version`: The version of the box (always 0).
/// - `flags`: Box flags (default 1, meaning 'quick rendering hint').
/// - `graphicsmode`: The transfer mode used (0 = copy mode by default).
/// - `opcolor`: Optional color used with specific graphics modes (default: [0, 0, 0]).
#[derive(Clone)]
pub struct VmhdBox { // Video Media Header Box
    pub version: u8,
    pub flags: u32,
    pub graphicsmode: u16,
    pub opcolor: [u16; 3],
}

impl Default for VmhdBox {
    fn default() -> Self {
        VmhdBox {
            version: 0,
            flags: 0x000001, // quick rendering hint
            graphicsmode: 0,
            opcolor: [0, 0, 0],
        }
    }
}

impl std::fmt::Debug for VmhdBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VmhdBox")
            .field("box_size", &self.box_size())
            .field("box_type", &format_fourcc(&self.box_type()))
            .field("version", &self.version)
            .field("flags", &format!("0x{:06X}", self.flags))
            .field("graphicsmode", &self.graphicsmode)
            .field("opcolor", &self.opcolor)
            .finish()
    }
}

impl Mp4Box for VmhdBox {
    fn box_type(&self) -> [u8; 4] { *b"vmhd" }

    fn box_size(&self) -> u32 {
        8 + 4 + 2 + 6  // header + version/flags + graphicsmode + opcolor (3*2)
    }

    fn write_box(&self, buffer: &mut Vec<u8>) {
        buffer.extend_from_slice(&self.box_size().to_be_bytes());
        buffer.extend_from_slice(&self.box_type());

        buffer.push(self.version);
        buffer.extend_from_slice(&(self.flags & 0x00FFFFFF).to_be_bytes()[1..]); // only 3 bytes for flags

        buffer.extend_from_slice(&self.graphicsmode.to_be_bytes());
        for color in &self.opcolor {
            buffer.extend_from_slice(&color.to_be_bytes());
        }
    }

    fn read_box(data: &[u8]) -> Result<(Self, usize), String> {
        let size = u32::from_be_bytes(data[0..4].try_into().unwrap()) as usize;
        if data.len() < size {
            return Err("Incomplete VMHD box".into());
        }
        if &data[4..8] != b"vmhd" {
            return Err("Not a VMHD box".into());
        }

        let version = data[8];
        let flags = u32::from_be_bytes([0, data[9], data[10], data[11]]);

        let graphicsmode = u16::from_be_bytes(data[12..14].try_into().unwrap());
        let opcolor = [
            u16::from_be_bytes(data[14..16].try_into().unwrap()),
            u16::from_be_bytes(data[16..18].try_into().unwrap()),
            u16::from_be_bytes(data[18..20].try_into().unwrap()),
        ];

        Ok((
            VmhdBox {
                version,
                flags,
                graphicsmode,
                opcolor,
            },
            size
        ))
    }
}
