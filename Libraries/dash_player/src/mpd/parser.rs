use crate::mpd::{AdaptationSet, MpdMetadata, Representation};
use quick_xml::events::Event;
use quick_xml::Reader;
use std::collections::HashMap;
use chrono::{DateTime, Utc};

#[allow(clippy::if_same_then_else)]
fn infer_content_type(mime_type: &str) -> &str {
    if mime_type.contains("audio") {
        "audio"
    } else if mime_type.contains("video") {
        "video"
    } else {
        "video" // fallback
    }
}

pub fn parse_mpd(xml: &str) -> Result<MpdMetadata, Box<dyn std::error::Error + Send + Sync>> {
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();
    let mut adaptation_sets = vec![];
    let mut availability_start_time = Utc::now();
    let mut time_shift_buffer_depth = None;
    let mut inside_rep = false;

    let mut current_adaptation: Option<AdaptationSet> = None;
    let mut current_rep: Option<Representation> = None;
    let mut adaptation_template: Option<HashMap<String, String>> = None;

    while let Ok(event) = reader.read_event_into(&mut buf) {
        match event {
            Event::Start(ref e) | Event::Empty(ref e) => {
                let name = e.name().to_owned();
                let tag = std::str::from_utf8(name.as_ref())?;

                match tag {
                    "MPD" => {
                        for attr in e.attributes() {
                            let attr = attr?;
                            let key = attr.key.as_ref();
                            let value = attr.unescape_value()?;
                            if key == b"availabilityStartTime" {
                                availability_start_time = value.parse::<DateTime<Utc>>()?;
                            }
                            if key == b"timeShiftBufferDepth" {
                                time_shift_buffer_depth = parse_duration(&value, 60.0);
                            }
                        }
                    }
                    "AdaptationSet" => {
                        let mut mime = String::new();
                        let mut content = String::new();

                        for attr in e.attributes() {
                            let attr = attr?;
                            match attr.key.as_ref() {
                                b"mimeType" => mime = attr.unescape_value()?.to_string(),
                                b"contentType" => content = attr.unescape_value()?.to_string(),
                                _ => {}
                            }
                        }

                        let fallback = infer_content_type(&mime).to_string();
                        current_adaptation = Some(AdaptationSet {
                            content_type: if !content.is_empty() { content } else { fallback },
                            mime_type: mime,
                            representations: vec![],
                            segment_template: None,
                        });
                    }
                    "Representation" => {
                        inside_rep = true;
                        let mut id = String::new();
                        let mut bandwidth = 0;
                        let mut availability_time_offset = None;
                        let mut availability_time_complete = None;

                        for attr in e.attributes() {
                            let attr = attr?;
                            match attr.key.as_ref() {
                                b"id" => id = attr.unescape_value()?.to_string(),
                                b"bandwidth" => {
                                    bandwidth = attr.unescape_value()?.parse::<u64>()?;
                                },
                                b"availabilityTimeOffset" => {
                                    availability_time_offset = attr.unescape_value()?.parse::<f64>().ok();
                                }
                                b"availabilityTimeComplete" => {
                                    availability_time_complete = attr.unescape_value()?.parse::<bool>().ok();
                                }
                                _ => {}
                            }
                        }

                        current_rep = Some(Representation {
                            id,
                            bandwidth,
                            initialization: String::new(),
                            media: String::new(),
                            segment_duration: 0.0,
                            timescale: 1,
                            uses_segment_time: false,
                            has_template: false,
                            availability_time_offset,
                            availability_time_complete,
                            presentation_time_offset: None,
                            segment_timeline: None,
                        });
                    }
                    "SegmentTemplate" => {
                        let mut map = HashMap::new();
                        for attr in e.attributes() {
                            let attr = attr?;
                            let key = std::str::from_utf8(attr.key.as_ref())?.to_string();
                            let value = attr.unescape_value()?.to_string();
                            map.insert(key, value);
                        }

                        if inside_rep {
                            if let Some(rep) = current_rep.as_mut() {

                                rep.initialization = map

                                    .get("initialization")

                                    .unwrap_or(&"".to_string())

                                    .replace("$RepresentationID$", &rep.id);

                                rep.media = map

                                    .get("media")

                                    .unwrap_or(&"".to_string())

                                    .replace("$RepresentationID$", &rep.id);



                                if let Some(dur) = map.get("duration") {

                                    rep.segment_duration = dur.parse::<f64>().unwrap_or(1.0);

                                }

                                if let Some(ts) = map.get("timescale") {

                                    rep.timescale = ts.parse::<u64>().unwrap_or(1);

                                }

                                if let Some(ato) = map.get("availabilityTimeOffset") {

                                    rep.availability_time_offset = ato.parse::<f64>().ok();

                                }

                                if let Some(atc) = map.get("availabilityTimeComplete") {

                                    rep.availability_time_complete = atc.parse::<bool>().ok();

                                }

                                if let Some(pto) = map.get("presentationTimeOffset") {

                                    rep.presentation_time_offset = pto.parse::<u64>().ok();

                                }



                                rep.uses_segment_time = rep.media.contains("$Time$");

                                rep.segment_duration /= rep.timescale as f64;

                                rep.has_template = true;

                            }
                        } else {
                            adaptation_template = Some(map);
                        }
                    }
                    _ => {}
                }
            }

            Event::End(ref e) => {
                let name = e.name().to_owned();
                let tag = std::str::from_utf8(name.as_ref())?;

                match tag {
                    "Representation" => {
                        inside_rep = false;
                        if let Some(mut rep) = current_rep.take() {
                            if let Some(template) = adaptation_template
                            .take() {
                                rep.initialization = template
                                    .get("initialization")
                                    .unwrap_or(&"".to_string())
                                    .replace("$RepresentationID$", &rep.id);
                                rep.media = template
                                    .get("media")
                                    .unwrap_or(&"".to_string())
                                    .replace("$RepresentationID$", &rep.id);

                                if let Some(dur) = template.get("duration") {
                                    rep.segment_duration = dur.parse::<f64>().unwrap_or(1.0);
                                }
                                if let Some(ts) = template.get("timescale") {
                                    rep.timescale = ts.parse::<u64>().unwrap_or(1);
                                }
                                if let Some(ato) = template.get("availabilityTimeOffset") {
                                    rep.availability_time_offset = ato.parse::<f64>().ok();
                                }
                                if let Some(atc) = template.get("availabilityTimeComplete") {
                                    rep.availability_time_complete = atc.parse::<bool>().ok();
                                }
                                if let Some(pto) = template.get("presentationTimeOffset") {
                                    rep.presentation_time_offset = pto.parse::<u64>().ok();
                                }

                                rep.uses_segment_time = rep.media.contains("$Time$");
                                rep.segment_duration /= rep.timescale as f64;
                                rep.has_template = true;
                            }

                            if let Some(adaptation) = current_adaptation.as_mut() {
                                adaptation.representations.push(rep);
                            }
                        }
                    }
                    "AdaptationSet" => {
                        if let Some(mut adapt) = current_adaptation.take() {
                            adapt.segment_template = adaptation_template.take();
                            for rep in adapt.representations.iter_mut() {
                                if !rep.has_template {
                                    rep.initialization = adapt.segment_template
                                        .as_ref()
                                        .and_then(|t| t.get("initialization"))
                                        .unwrap_or(&"".to_string())
                                        .replace("$RepresentationID$", &rep.id);
                                    rep.media = adapt.segment_template
                                        .as_ref()
                                        .and_then(|t| t.get("media"))
                                        .unwrap_or(&"".to_string())
                                        .replace("$RepresentationID$", &rep.id);
                                    rep.segment_duration = adapt.segment_template
                                        .as_ref()
                                        .and_then(|t| t.get("duration"))
                                        .and_then(|d| d.parse::<f64>().ok())
                                        .unwrap_or(1.0);
                                    rep.timescale = adapt.segment_template
                                        .as_ref()
                                        .and_then(|t| t.get("timescale"))
                                        .and_then(|ts| ts.parse::<u64>().ok())
                                        .unwrap_or(1);
                                    rep.uses_segment_time = rep.media.contains("$Time$");
                                    rep.segment_duration /= rep.timescale as f64;
                                    rep.has_template = true;
                                }

                                if rep.segment_duration == 0.0 {
                                    rep.segment_duration = 1.0;
                                }
                            }
                            adaptation_sets.push(adapt);
                        }
                    }
                    _ => {}
                }
            }

            Event::Eof => break,
            _ => {}
        }

        buf.clear();
    }

    Ok(MpdMetadata {
        availability_start_time,
        adaptation_sets,
        time_shift_buffer_depth,
    })
}

fn parse_duration(value: &str, fallback_seconds: f64) -> Option<f64> {
    let iso = iso8601_duration::Duration::parse(value).ok()?;
    let seconds = iso.to_std()
        .or_else(|| Some(std::time::Duration::from_secs_f64(fallback_seconds)))
        .map(|d| d.as_secs_f64())?;
    Some(seconds)
}
