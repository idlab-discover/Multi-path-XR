use crate::boxes::{ftyp::FtypBox, generic::Mp4Box, mdat::MdatBox, moof::MoofBox, moov::MoovBox, styp::StypBox, traf::TrafBox, trak::TrakBox, vmhd::VmhdBox};

#[derive(Clone, Debug)]
pub struct Mp4StreamConfig {
    pub track_id: u32,                  // Unique track identifier
    pub timescale: u32,                 // Typically fps * 1000
    pub default_sample_duration: u32,   // e.g., 1000 for fixed frame durations
    pub codec_fourcc: [u8; 4],          // Custom codec, e.g., *b\"pcvc\"
    pub codec_name: String,             // Descriptive codec name
    pub width: u16,                     // Video width in pixels
    pub height: u16,                    // Video height in pixels
}


pub fn create_init_segment(config: &Mp4StreamConfig) -> Vec<u8> {
    let mut buffer = Vec::with_capacity(2048);  // Pre-allocate for efficiency

    // 1) Write FTYP Box
    let ftyp = FtypBox::default();
    ftyp.write_box(&mut buffer);

    // 2) Prepare MOOV Box with overrides
    let mut moov = MoovBox::default();

    // --- Override mvhd ---
    moov.mvhd.timescale = config.timescale;
    moov.mvhd.duration = 3510080100; // A very long duration for testing

    // --- Override tkhd ---
    moov.traks.push(TrakBox::default());
    moov.traks[0].tkhd.track_id = config.track_id;
    moov.traks[0].tkhd.width = (config.width as u32) << 16;
    moov.traks[0].tkhd.height = (config.height as u32) << 16;
    moov.traks[0].mdia.minf.vmhd = Some(VmhdBox::default());

    // --- Override mdhd ---
    moov.traks[0].mdia.mdhd.timescale = config.timescale;

    // --- Override stsd / codec info ---
    let stsd = &mut moov.traks[0].mdia.minf.stbl.stsd;
    if let Some(entry) = stsd.entries.get_mut(0) {
        entry.data_format = config.codec_fourcc;
        entry.width = config.width;
        entry.height = config.height;
        entry.compressor_name = config.codec_name.clone();
    }

    // --- Override trex ---
    if let Some(mvex) = moov.mvex.as_mut() {
        if let Some(trex) = mvex.trex_entries.get_mut(0) {
            trex.track_id = config.track_id;
            trex.default_sample_duration = config.default_sample_duration;
        }
    }

    // 3) Write MOOV Box
    moov.write_box(&mut buffer);

    buffer
}


pub fn create_media_segment(
    config: &Mp4StreamConfig,
    frame_data: &[u8],
    sequence_number: u32,
    base_decode_time: u64
) -> Vec<u8> {
    let mut segment = Vec::new();

    // 1) Write STYP Box
    let styp = StypBox::default();
    styp.write_box(&mut segment);

    // 2) Initialize MOOF Box with defaults
    let mut moof = MoofBox::default();

    // -- Set dynamic fields --
    moof.mfhd.sequence_number = sequence_number;
    moof.trafs.push(TrafBox::default());
    moof.trafs[0].tfhd.track_id = config.track_id;
    if let Some (tfdt) = moof.trafs[0].tfdt.as_mut() {
        tfdt.base_decode_time = base_decode_time;
    }

    if let Some(trun) = moof.trafs[0].trun.as_mut() {
        trun.sample_size = frame_data.len() as u32;

        // Placeholder for data_offset for now
        trun.data_offset = 0;
    }

    // 3) Serialize MOOF to temporary buffer
    let mut moof_buffer = Vec::new();
    moof.write_box(&mut moof_buffer);

    if let Some(trun) = moof.trafs[0].trun.as_mut() {
        // 4) Calculate correct data_offset
        let data_offset = moof_buffer.len() as i32 + 8;  // 8 bytes for mdat header
        // Update trun.data_offset
        trun.data_offset = data_offset;
        // 5) Re-serialize MOOF with correct offset
        moof_buffer.clear();
        moof.write_box(&mut moof_buffer);
    }

    // 6) Create MDAT Box
    let mdat = MdatBox {
        data: frame_data.to_vec(),  // Copy frame data into MDAT
    };
    let mut mdat_buffer = Vec::new();
    mdat.write_box(&mut mdat_buffer);

    // 7) Combine MOOF + MDAT
    segment.extend_from_slice(&moof_buffer);
    segment.extend_from_slice(&mdat_buffer);

    segment
}
