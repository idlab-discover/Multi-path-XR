use crate::boxes::{co64::Co64Box, ctts::CttsBox, dinf::DinfBox, dref::DrefBox, edts::EdtsBox, elst::ElstBox, enums::Mp4BoxEnum, ftyp::FtypBox, generic::{Mp4Box, UnknownBox}, hdlr::HdlrBox, mdat::MdatBox, mdhd::MdhdBox, mdia::MdiaBox, mehd::MehdBox, meta::MetaBox, mfhd::MfhdBox, minf::MinfBox, moof::MoofBox, moov::MoovBox, mvex::MvexBox, mvhd::MvhdBox, smhd::SmhdBox, stbl::StblBox, stco::StcoBox, stsc::StscBox, stsd::StsdBox, stss::StssBox, stsz::StszBox, stts::SttsBox, styp::StypBox, tfdt::TfdtBox, tfhd::TfhdBox, tkhd::TkhdBox, traf::TrafBox, trak::TrakBox, trex::TrexBox, trun::TrunBox, udta::UdtaBox, vmhd::VmhdBox};

pub fn extract_mdat_boxes(mut data: &[u8]) -> Result<Vec<MdatBox>, String> {
    let mut mdat_boxes = Vec::new();

    while data.len() >= 8 {
        let size = u32::from_be_bytes(data[0..4].try_into().unwrap()) as usize;
        let box_type = &data[4..8];

        if size == 0 || size > data.len() {
            return Err(format!("Corrupted MP4 box size of box: {:?}, reported size: {}, actual size: {}, we have {} boxes", box_type, size, data.len(), mdat_boxes.len()));
        }

        if box_type == b"mdat" {
            // Properly parse the mdat box
            let payload = data[8..size].to_vec();
            mdat_boxes.push(MdatBox { data: payload });
        }

        // Move to the next box
        data = &data[size..];
    }

    if !data.is_empty() {
        return Err("Trailing incomplete box at end of buffer".into());
    }

    Ok(mdat_boxes)
}

pub fn parse_mp4_boxes(mut data: &[u8]) -> Result<Vec<Mp4BoxEnum>, String> {
    let mut boxes = Vec::new();

    while !data.is_empty() {
        if data.len() < 8 {
            return Err("Remaining data too small for MP4 box header".into());
        }

        let (mp4_box, consumed) = read_mp4_box(data)?;

        boxes.push(mp4_box);

        if consumed == 0 || consumed > data.len() {
            return Err("Invalid box size detected".into());
        }

        data = &data[consumed..];
    }

    Ok(boxes)
}

pub fn read_mp4_box(data: &[u8]) -> Result<(Mp4BoxEnum, usize), String> {
    if data.len() < 8 {
        return Err("Buffer too small for MP4 box header".into());
    }

    let box_type = &data[4..8];

    match box_type {
        // TODO: Add more box types as needed
        b"co64" => Co64Box::read_box(data).map(|(b, s)| (Mp4BoxEnum::Co64(b), s)),
        b"ctts" => CttsBox::read_box(data).map(|(b, s)| (Mp4BoxEnum::Ctts(b), s)),
        b"dinf" => DinfBox::read_box(data).map(|(b, s)| (Mp4BoxEnum::Dinf(b), s)),
        b"dref" => DrefBox::read_box(data).map(|(b, s)| (Mp4BoxEnum::Dref(b), s)),
        b"edts" => EdtsBox::read_box(data).map(|(b, s)| (Mp4BoxEnum::Edts(b), s)),
        b"elst" => ElstBox::read_box(data).map(|(b, s)| (Mp4BoxEnum::Elst(b), s)),
        b"ftyp" => FtypBox::read_box(data).map(|(b, s)| (Mp4BoxEnum::Ftyp(b), s)),
        b"hdlr" => HdlrBox::read_box(data).map(|(b, s)| (Mp4BoxEnum::Hdlr(b), s)),
        b"mdat" => MdatBox::read_box(data).map(|(b, s)| (Mp4BoxEnum::Mdat(b), s)),
        b"mdhd" => MdhdBox::read_box(data).map(|(b, s)| (Mp4BoxEnum::Mdhd(b), s)),
        b"mdia" => MdiaBox::read_box(data).map(|(b, s)| (Mp4BoxEnum::Mdia(b), s)),
        b"mehd" => MehdBox::read_box(data).map(|(b, s)| (Mp4BoxEnum::Mehd(b), s)),
        b"meta" => MetaBox::read_box(data).map(|(b, s)| (Mp4BoxEnum::Meta(b), s)),
        b"mfhd" => MfhdBox::read_box(data).map(|(b, s)| (Mp4BoxEnum::Mfhd(b), s)),
        b"minf" => MinfBox::read_box(data).map(|(b, s)| (Mp4BoxEnum::Minf(b), s)),
        b"moof" => MoofBox::read_box(data).map(|(b, s)| (Mp4BoxEnum::Moof(b), s)),
        b"moov" => MoovBox::read_box(data).map(|(b, s)| (Mp4BoxEnum::Moov(b), s)),
        b"mvex" => MvexBox::read_box(data).map(|(b, s)| (Mp4BoxEnum::Mvex(b), s)),
        b"mvhd" => MvhdBox::read_box(data).map(|(b, s)| (Mp4BoxEnum::Mvhd(b), s)),
        b"smhd" => SmhdBox::read_box(data).map(|(b, s)| (Mp4BoxEnum::Smhd(b), s)),
        b"stbl" => StblBox::read_box(data).map(|(b, s)| (Mp4BoxEnum::Stbl(b), s)),
        b"stco" => StcoBox::read_box(data).map(|(b, s)| (Mp4BoxEnum::Stco(b), s)),
        b"stsc" => StscBox::read_box(data).map(|(b, s)| (Mp4BoxEnum::Stsc(b), s)),
        b"stsd" => StsdBox::read_box(data).map(|(b, s)| (Mp4BoxEnum::Stsd(b), s)),
        b"stss" => StssBox::read_box(data).map(|(b, s)| (Mp4BoxEnum::Stss(b), s)),
        b"stsz" => StszBox::read_box(data).map(|(b, s)| (Mp4BoxEnum::Stsz(b), s)),
        b"stts" => SttsBox::read_box(data).map(|(b, s)| (Mp4BoxEnum::Stts(b), s)),
        b"styp" => StypBox::read_box(data).map(|(b, s)| (Mp4BoxEnum::Styp(b), s)),
        b"tfdt" => TfdtBox::read_box(data).map(|(b, s)| (Mp4BoxEnum::Tfdt(b), s)),
        b"tfhd" => TfhdBox::read_box(data).map(|(b, s)| (Mp4BoxEnum::Tfhd(b), s)),
        b"tkhd" => TkhdBox::read_box(data).map(|(b, s)| (Mp4BoxEnum::Tkhd(b), s)),
        b"traf" => TrafBox::read_box(data).map(|(b, s)| (Mp4BoxEnum::Traf(b), s)),
        b"trak" => TrakBox::read_box(data).map(|(b, s)| (Mp4BoxEnum::Trak(b), s)),
        b"trex" => TrexBox::read_box(data).map(|(b, s)| (Mp4BoxEnum::Trex(b), s)),
        b"trun" => TrunBox::read_box(data).map(|(b, s)| (Mp4BoxEnum::Trun(b), s)),
        b"udta" => UdtaBox::read_box(data).map(|(b, s)| (Mp4BoxEnum::Udta(b), s)),
        b"vmhd" => VmhdBox::read_box(data).map(|(b, s)| (Mp4BoxEnum::Vmhd(b), s)),
        _ => {
            // Fallback to UnknownBox
            let size = u32::from_be_bytes(data[0..4].try_into().unwrap()) as usize;
            if data.len() < size {
                return Err("Incomplete unknown box".into());
            }
            let unknown = UnknownBox {
                btype: box_type.try_into().unwrap(),
                data: data[8..size].to_vec(),
            };
            Ok((Mp4BoxEnum::Unknown(unknown), size))
        }
    }
}