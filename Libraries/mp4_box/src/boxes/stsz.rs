use crate::format_fourcc;

use super::generic::Mp4Box;

// The `StszBox` struct represents a Sample Size Box in the MP4 file format.
// This box contains information about the size of each sample in the media data.
// It includes a default sample size (if all samples are the same size) and a table of sample sizes (if samples have varying sizes).
//
// Fields:
// - `version`: The version of the box.
// - `flags`: Flags indicating specific properties of the box.
// - `sample_size`: The default sample size. If this value is 0, the sample sizes are specified in the `entry_sizes` field.
// - `sample_count`: The number of samples in the media data.
// - `entry_sizes`: A vector of sample sizes. If `sample_size` is 0, this vector contains the sizes of each sample.
//   If `sample_size` is not 0, this vector is empty.
//   The `entry_sizes` field is used when the sample sizes are not uniform.
//
// The `StszBox` is essential for enabling efficient access to media samples, as it provides the size of each sample,
// which is required to locate and decode the samples in the media data.
#[derive(Clone)]
pub struct StszBox { // Sample Size Box
    pub version: u8,
    pub flags: u32,
    pub sample_size: u32, // Default sample size
    pub entry_sizes: Vec<u32>, // List of sample sizes
}

// Provides a default implementation for the `StszBox` struct.
// The default `StszBox` represents an empty box with no entries.
impl Default for StszBox {
    fn default() -> Self {
        StszBox {
            version: 0,
            flags: 0,
            sample_size: 0,
            entry_sizes: vec![
                0,
            ],
        }
    }
}

impl std::fmt::Debug for StszBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StszBox")
            .field("box_size", &self.box_size())
            .field("box_type", &format_fourcc(&self.box_type()))
            .field("version", &self.version)
            .field("flags", &self.flags)
            .field("sample_size", &self.sample_size)
            .field("entry_sizes", &self.entry_sizes)
            .finish()
    }
}

// Implementation of the `Mp4Box` trait for the `StszBox` struct.
impl Mp4Box for StszBox {
    // Returns the box type as a 4-byte array. For `StszBox`, the type is "stsz".
    fn box_type(&self) -> [u8; 4] { *b"stsz" }

    // Calculates the size of the `StszBox` in bytes.
    // The size is fixed at 20 bytes, which includes:
    // - 8 bytes for the header (4 bytes for size and 4 bytes for type).
    // - 4 bytes for the version and flags.
    // - 4 bytes for the `sample_size` field, which is set to 0 in this implementation.
    // - 4 bytes for the `sample_count` field, which is set to 0 in this implementation.
    // - 4 bytes for each entry size in the `entry_sizes` vector.
    fn box_size(&self) -> u32 {
        let base = 8 + 4 + 4 + 4;
        if self.sample_size == 0 {
            base + (4 * self.entry_sizes.len() as u32)
        } else {
            base
        }
    }

    // Writes the `StszBox` to the provided buffer.
    // The method serializes the box size, box type, version, flags, `sample_size`, and `sample_count` into the buffer.
    fn write_box(&self, buffer: &mut Vec<u8>) {
        buffer.extend_from_slice(&self.box_size().to_be_bytes());
        buffer.extend_from_slice(&self.box_type());
        buffer.push(self.version);
        buffer.extend_from_slice(&self.flags.to_be_bytes()[1..4]);  // flags (24 bits)
        buffer.extend_from_slice(&self.sample_size.to_be_bytes());
        if self.sample_size == 0 {
            buffer.extend_from_slice(&(self.entry_sizes.len() as u32).to_be_bytes());
            for entry in &self.entry_sizes {
                buffer.extend_from_slice(&entry.to_be_bytes());
            }
        } else {
            buffer.extend_from_slice(&(0u32).to_be_bytes());  // sample_count = 0 when default sample_size > 0
        }
    }

    fn read_box(data: &[u8]) -> Result<(Self, usize), String> {
        let size = u32::from_be_bytes(data[0..4].try_into().unwrap()) as usize;
        if data.len() < size {
            return Err("Incomplete STSZ box".into());
        }
        if &data[4..8] != b"stsz" {
            return Err("Not an STSZ box".into());
        }

        let version = data[8];
        let mut flag_bytes = [0u8; 4];
        flag_bytes[1..4].copy_from_slice(&data[9..12]);
        let flags = u32::from_be_bytes(flag_bytes);

        let sample_size = u32::from_be_bytes(data[12..16].try_into().unwrap());
        let sample_count = u32::from_be_bytes(data[16..20].try_into().unwrap());
    
        let mut entry_sizes = Vec::new();
        if sample_size == 0 {
            let mut offset = 20;
            for _ in 0..sample_count {
                let entry = u32::from_be_bytes(data[offset..offset+4].try_into().unwrap());
                entry_sizes.push(entry);
                offset += 4;
            }
        }
    
        Ok((StszBox { sample_size, entry_sizes, version, flags }, size))
    }
}
