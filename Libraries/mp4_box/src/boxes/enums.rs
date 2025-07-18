use super::{co64::Co64Box, ctts::CttsBox, dinf::DinfBox, dref::DrefBox, edts::EdtsBox, elst::ElstBox, ftyp::FtypBox, generic::UnknownBox, hdlr::HdlrBox, mdat::MdatBox, mdhd::MdhdBox, mdia::MdiaBox, mehd::MehdBox, meta::MetaBox, mfhd::MfhdBox, minf::MinfBox, moof::MoofBox, moov::MoovBox, mvex::MvexBox, mvhd::MvhdBox, smhd::SmhdBox, stbl::StblBox, stco::StcoBox, stsc::StscBox, stsd::StsdBox, stss::StssBox, stsz::StszBox, stts::SttsBox, styp::StypBox, tfdt::TfdtBox, tfhd::TfhdBox, tkhd::TkhdBox, traf::TrafBox, trak::TrakBox, trex::TrexBox, trun::TrunBox, udta::UdtaBox, vmhd::VmhdBox};

#[derive(Debug, Clone)]
pub enum Mp4BoxEnum {
    Co64(Co64Box),
    Ctts(CttsBox),
    Dinf(DinfBox),
    Dref(DrefBox),
    Edts(EdtsBox),
    Elst(ElstBox),
    Ftyp(FtypBox),
    Hdlr(HdlrBox),
    Mdat(MdatBox),
    Mdhd(MdhdBox),
    Mdia(MdiaBox),
    Mehd(MehdBox),
    Meta(MetaBox),
    Mfhd(MfhdBox),
    Minf(MinfBox),
    Moof(MoofBox),
    Moov(MoovBox),
    Mvex(MvexBox),
    Mvhd(MvhdBox),
    Smhd(SmhdBox),
    Stbl(StblBox),
    Stco(StcoBox),
    Stsc(StscBox),
    Stsd(StsdBox),
    Stss(StssBox),
    Stsz(StszBox),
    Stts(SttsBox),
    Styp(StypBox),
    Tfdt(TfdtBox),
    Tfhd(TfhdBox),
    Tkhd(TkhdBox),
    Traf(TrafBox),
    Trak(TrakBox),
    Trex(TrexBox),
    Trun(TrunBox),
    Udta(UdtaBox),
    Vmhd(VmhdBox),
    Unknown(UnknownBox),
}
