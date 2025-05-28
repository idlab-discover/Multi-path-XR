use chrono::{DateTime, Utc};
use quick_xml::events::{BytesEnd, BytesStart, Event};
use quick_xml::Writer;
use std::io::Cursor;

/// One representation within an adaptation set
#[derive(Debug, Clone)]
pub struct RepresentationDef {
    pub id: String,
    pub mime_type: String,
    pub codecs: String,
    pub bandwidth: u64,
    pub initialization: String,
    pub media: String,
    pub availability_time_offset: Option<f64>,
    pub availability_time_complete: Option<bool>,
}

/// Main MPD builder
#[derive(Debug, Clone)]
pub struct MpdBuilder {
    pub availability_start_time: DateTime<Utc>,
    pub time_shift_buffer_depth: f64,
    pub minimum_update_period: Option<f64>,
    pub suggested_presentation_delay: Option<f64>,
    pub segment_duration: u64,
    pub timescale: u64,
    pub representations: Vec<RepresentationDef>,
}

impl MpdBuilder {
    pub fn live() -> Self {
        Self {
            availability_start_time: Utc::now(),
            time_shift_buffer_depth: 60.0,
            minimum_update_period: Some(5.0),
            suggested_presentation_delay: Some(2.0),
            segment_duration: 1,
            timescale: 1,
            representations: vec![],
        }
    }

    /**
     * Set the availability start time for the MPD.
     * This is the time when the first segment is available for playback.
     */
    pub fn availability_start(mut self, time: DateTime<Utc>) -> Self {
        self.availability_start_time = time;
        self
    }

    /**
     * Set the time shift buffer depth for the MPD.
     * This is the amount of time that the player can rewind in the live stream.
     * It is specified in seconds.
     */
    pub fn time_shift_buffer(mut self, seconds: f64) -> Self {
        self.time_shift_buffer_depth = seconds;
        self
    }

    /**
     * Set the segment duration and timescale for the MPD.
     * The segment duration is the length of each segment in seconds.
     * The timescale is the number of time units per second.
     */
    pub fn segment_duration(mut self, duration: u64, timescale: u64) -> Self {
        self.segment_duration = duration;
        self.timescale = timescale;
        self
    }

    /**
     * Set the minimum update period for the MPD.
     * This is the minimum time between updates to the MPD.
     */
    pub fn minimum_update_period(mut self, seconds: f64) -> Self {
        self.minimum_update_period = Some(seconds);
        self
    }

    /**
     * Set the suggested presentation delay for the MPD.
     * This is the amount of time that the player should wait before starting playback.
     */
    pub fn suggested_presentation_delay(mut self, seconds: f64) -> Self {
        self.suggested_presentation_delay = Some(seconds);
        self
    }

    /**
     * Add a new representation to the MPD.
     * Each representation is a different quality level of the same content.
     * The ID is a unique identifier for the representation.
     * The mime type is the media type of the representation (e.g., "video/mp4").
     * The codecs are the codecs used to encode the representation (e.g., "avc1.42E01E").
     * The bandwidth is the bitrate of the representation in bits per second.
     * The initialization is the URL of the initialization segment for the representation.
     * The media is the URL of the media segments for the representation.
     * The media segments are numbered using the $Number%09d$ placeholder.
     */
    pub fn add_representation(
        mut self,
        id: &str,
        mime_type: &str,
        codecs: &str,
        bandwidth: u64,
        initialization: &str,
        media: &str,
        availability_time_offset: Option<f64>,
        availability_time_complete: Option<bool>,
    ) -> Self {
        // Add the new representation
        self.representations.push(RepresentationDef {
            id: id.to_string(),
            mime_type: mime_type.to_string(),
            codecs: codecs.to_string(),
            bandwidth,
            initialization: initialization.to_string(),
            media: media.to_string(),
            availability_time_offset,
            availability_time_complete,
        });
        self
    }

    /**
     * Build the MPD XML string.
     * This function generates the XML representation of the MPD based on the
     * properties set in the builder.
     */
    pub fn build_xml_string(&self) -> Result<String, Box<dyn std::error::Error>> {
        let mut writer = Writer::new(Cursor::new(Vec::new()));

        let mut mpd = BytesStart::new("MPD");
        mpd.push_attribute(("xmlns", "urn:mpeg:dash:schema:mpd:2011"));
        mpd.push_attribute(("type", "dynamic"));
        mpd.push_attribute((
            "availabilityStartTime",
            self.availability_start_time.to_rfc3339().as_str(),
        ));
        mpd.push_attribute((
            "timeShiftBufferDepth",
            format!("PT{}S", self.time_shift_buffer_depth).as_str(),
        ));
        if let Some(v) = self.minimum_update_period {
            mpd.push_attribute(("minimumUpdatePeriod", format!("PT{}S", v).as_str()));
        }
        if let Some(v) = self.suggested_presentation_delay {
            mpd.push_attribute(("suggestedPresentationDelay", format!("PT{}S", v).as_str()));
        }

        writer.write_event(Event::Start(mpd))?;

        writer.write_event(Event::Start(BytesStart::new("Period")))?;

        let mut adaptation = BytesStart::new("AdaptationSet");
        if let Some(first) = self.representations.first() {
            adaptation.push_attribute(("mimeType", first.mime_type.as_str()));
        }
        writer.write_event(Event::Start(adaptation))?;

        for rep in &self.representations {
            let mut rep_el = BytesStart::new("Representation");
            rep_el.push_attribute(("id", rep.id.as_str()));
            rep_el.push_attribute(("bandwidth", rep.bandwidth.to_string().as_str()));
            rep_el.push_attribute(("codecs", rep.codecs.as_str()));

            if let Some(ato) = rep.availability_time_offset {
                rep_el.push_attribute(("availabilityTimeOffset", ato.to_string().as_str()));
            }
            if let Some(atc) = rep.availability_time_complete {
                rep_el.push_attribute(("availabilityTimeComplete", atc.to_string().as_str()));
            }

            writer.write_event(Event::Start(rep_el))?;

            let mut template = BytesStart::new("SegmentTemplate");
            template.push_attribute(("timescale", self.timescale.to_string().as_str()));
            template.push_attribute(("duration", self.segment_duration.to_string().as_str()));
            template.push_attribute(("initialization", rep.initialization.as_str()));
            template.push_attribute(("media", rep.media.as_str()));
            writer.write_event(Event::Empty(template))?;

            writer.write_event(Event::End(BytesEnd::new("Representation")))?;
        }

        writer.write_event(Event::End(BytesEnd::new("AdaptationSet")))?;
        writer.write_event(Event::End(BytesEnd::new("Period")))?;
        writer.write_event(Event::End(BytesEnd::new("MPD")))?;

        let result = writer.into_inner().into_inner();
        Ok(String::from_utf8(result)?)
    }
}
