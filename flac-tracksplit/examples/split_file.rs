use std::{fs::File, path::PathBuf, str::FromStr};

use symphonia_bundle_flac::FlacReader;
use symphonia_core::{
    formats::{Cue, FormatReader},
    io::MediaSourceStream,
    meta::{Tag, Value, Visual},
};
use symphonia_utils_xiph::flac::metadata::StreamInfo;

fn main() {
    let file = File::open("test.flac").expect("opening test.flac");
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut reader = FlacReader::try_new(mss, &Default::default()).expect("creating flac reader");
    // println!("tracks: {:?}", reader.tracks());
    if let Some(data) = &reader
        .default_track()
        .expect("default track")
        .codec_params
        .extra_data
    {
        let info = StreamInfo::read(&mut symphonia_core::io::BufReader::new(&data))
            .expect("parse STREAMINFO");
        println!("streaminfo: {:?}", info);
    }
    let cues: Vec<Cue> = reader.cues().iter().cloned().collect();
    println!("cues: {:?}", cues);
    let metadata = reader.metadata();
    let current_metadata = metadata.current().expect("tags");
    let tags = current_metadata.tags();
    let visuals = current_metadata.visuals();

    let mut cue_iter = cues.iter().peekable();
    while let Some(cue) = cue_iter.next() {
        let mut next = cue_iter.peek();
        if let Some(LEAD_OUT_TRACK_NUMBER) = next.map(|n| n.index) {
            // we have a lead-out, capture the whole in the last track.
            next = None;
        }
        let track = Track::from_tags(cue, next, &tags, &visuals);
        println!(
            "\ntrack {}: {:?} = {:?}",
            track.number,
            track.pathname(),
            track
        );
        if next.is_none() {
            break;
        }
    }
    println!("first packet ts: {:?}", reader.next_packet().unwrap().pts());
    println!(
        "second packet ts: {:?}",
        reader.next_packet().unwrap().pts()
    );
}

const LEAD_OUT_TRACK_NUMBER: u32 = 170;

#[derive(Clone)]
struct Track {
    number: u32,
    start_ts: u64,
    end_ts: Option<u64>,
    tags: Vec<Tag>,
    visuals: Vec<Visual>,
}

impl std::fmt::Debug for Track {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Track")
            .field("number", &self.number)
            .field("start_ts", &self.start_ts)
            .field("end_ts", &self.end_ts)
            .field("tags", &self.tags)
            .finish()
    }
}

impl Track {
    fn interesting_tag(name: &str) -> bool {
        !name.ends_with("]") && name != "CUESHEET" && name != "LOG"
    }

    fn from_tags(cue: &Cue, next: Option<&&Cue>, tags: &[Tag], visuals: &[Visual]) -> Self {
        let suffix = format!("[{}]", cue.index);
        let tags = tags
            .into_iter()
            .filter_map(|tag| {
                let tag_name = if tag.key.ends_with(&suffix) {
                    Some(&tag.key[0..(tag.key.len() - suffix.len())])
                } else if Self::interesting_tag(&tag.key) {
                    Some(tag.key.as_str())
                } else {
                    None
                };
                tag_name.map(|key| Tag::new(tag.std_key, key, tag.value.clone()))
            })
            .collect();
        let visuals = visuals.into_iter().cloned().collect();
        Self {
            number: cue.index,
            start_ts: cue.start_ts,
            end_ts: next.map(|cue| cue.start_ts),
            tags,
            visuals,
        }
    }

    fn tag_value(&self, name: &str) -> Option<&Value> {
        self.tags
            .iter()
            .find(|tag| tag.key == name)
            .map(|found| &found.value)
    }

    fn pathname(&self) -> PathBuf {
        let mut buf = PathBuf::new();
        if let Some(Value::String(artist)) = self.tag_value("ALBUMARTIST") {
            buf.push(artist);
        } else if let Some(Value::String(artist)) = self.tag_value("ARTIST") {
            buf.push(artist);
        } else {
            buf.push("Unknown Artist");
        }

        if let Some(Value::String(album)) = self.tag_value("ALBUM") {
            if let Some(Value::String(year)) = self.tag_value("DATE") {
                buf.push(format!("{} - {}", year, album));
            } else {
                buf.push(album);
            }
        } else {
            buf.push("Unknown Album");
        }

        if let (Some(Value::String(track)), Some(Value::String(title))) =
            (self.tag_value("TRACKNUMBER"), self.tag_value("TITLE"))
        {
            if let Ok(trackno) = <usize as FromStr>::from_str(track) {
                buf.push(format!("{:02}.{}.flac", trackno, title));
            } else {
                buf.push(format!("99.{}.flac", title));
            }
        }
        buf
    }
}
