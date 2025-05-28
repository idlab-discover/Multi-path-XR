// This module contains definitions for various MP4 box types used in the MP4 file format.
// MP4 boxes are the fundamental building blocks of the MP4 file structure, and each box
// serves a specific purpose, such as storing metadata, media data, or structural information.
//
// The following submodules are included:
//
// - `co64`: Defines the Chunk Offset 64 Box, which specifies the location of chunks in the media data.
// - `ctts`: Defines the Composition Time-to-Sample Box, which maps decoding times to samples.
// - `dinf`: Defines the Data Information Box, which holds information about data references.
// - `dref`: Defines the Data Reference Box, which specifies the location of media data.
// - `edts`: Defines the Edit Box, which contains information about how to map the media time-line to the presentation time-line.
// - `elst`: Defines the Edit List Box, which defines the mapping from media time to presentation time.
// - `ftyp`: Defines the File Type Box, which specifies the file type and compatibility information.
// - `generic`: Contains the `Mp4Box` trait, which provides a common interface for all MP4 boxes.
// - `hdlr`: Defines the Handler Reference Box, which specifies the type of media and handler name.
// - `mdat`: Defines the Media Data Box, which contains the raw media data.
// - `mdhd`: Defines the Media Header Box, which contains metadata about the media, such as timescale and duration.
// - `mdia`: Defines the Media Box, which is a container for media-specific information.
// - `mehd`: Defines the Movie Extends Header Box, which specifies the duration of the movie fragment.
// - `meta`: Defines the metadata Box, which provides metadata information for the entire movie.
// - `mfhd`: Defines the Movie Fragment Header Box, which provides information about movie fragments.
// - `minf`: Defines the Media Information Box, which contains media-specific information.
// - `mvex`: Defines the Movie Extends Box, which provides information for movie fragments.
// - `moof`: Defines the Movie Fragment Box, which contains a fragment of the movie.
// - `moov`: Defines the Movie Box, which contains metadata for the entire movie.
// - `mvhd`: Defines the Movie Header Box, which contains global information about the movie.
// - `smhd`: Defines the Sound Media Header Box, which contains sound-specific information.
// - `stbl`: Defines the Sample Table Box, which contains detailed information about media samples.
// - `stco`: Defines the Chunk Offset Box, which specifies the location of chunks in the media data.
// - `stsc`: Defines the Sample-to-Chunk Box, which maps samples to chunks.
// - `stsd`: Defines the Sample Description Box, which describes the format of media samples.
// - `stss`: Defines the Sync Sample Box, which specifies sync samples in the media stream.
// - `stsz`: Defines the Sample Size Box, which specifies the size of each sample.
// - `stts`: Defines the Time-to-Sample Box, which maps decoding times to samples.
// - `styp`: Defines the Segment Type Box, which specifies the segment type and compatibility information.
// - `tfdt`: Defines the Track Fragment Decode Time Box, which specifies the decode time of a track fragment.
// - `tfhd`: Defines the Track Fragment Header Box, which provides information about a track fragment.
// - `traf`: Defines the Track Fragment Box, which contains a fragment of a track.
// - `tkhd`: Defines the Track Header Box, which contains metadata about a track.
// - `trak`: Defines the Track Box, which is a container for track-specific information.
// - `trex`: Defines the Track Extends Box, which provides default values for track fragments.
// - `trun`: Defines the Track Run Box, which specifies the samples in a track fragment.
// - `udta`: Defines the User Data Box, which contains user-specific data.
// - `vmhd`: Defines the Video Media Header Box, which contains video-specific information.

pub mod co64;
pub mod ctts;
pub mod dinf;
pub mod dref;
pub mod edts;
pub mod elst;
pub mod enums;
pub mod ftyp;
pub mod generic;
pub mod hdlr;
pub mod mdat;
pub mod mdhd;
pub mod mdia;
pub mod mehd;
pub mod meta;
pub mod mfhd;
pub mod minf;
pub mod mvex;
pub mod moof;
pub mod moov;
pub mod mvhd;
pub mod smhd;
pub mod stbl;
pub mod stco;
pub mod stsc;
pub mod stsd;
pub mod stss;
pub mod stsz;
pub mod stts;
pub mod styp;
pub mod tfdt;
pub mod tfhd;
pub mod traf;
pub mod tkhd;
pub mod trak;
pub mod trex;
pub mod trun;
pub mod udta;
pub mod vmhd;