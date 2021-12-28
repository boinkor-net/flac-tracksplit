use std::fs::File;

use symphonia_bundle_flac::FlacReader;
use symphonia_core::{
    formats::{Cue, FormatReader},
    io::MediaSourceStream,
    meta::Tag,
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
    let tags = metadata.current().expect("tags").tags();

    let mut cue_iter = cues.iter().peekable();
    while let Some(cue) = cue_iter.next() {
        let mut next = cue_iter.peek();
        if let Some(LEAD_OUT_TRACK_NUMBER) = next.map(|n| n.index) {
            // we have a lead-out, capture the whole in the last track.
            next = None;
        }
        let track = Track::from_tags(cue, next, &tags);
        println!("\ntrack {}: {:?}", track.number, track);
        if next.is_none() {
            break;
        }
    }
}

const LEAD_OUT_TRACK_NUMBER: u32 = 170;

#[derive(Clone, Debug)]
struct Track {
    number: u32,
    start_ts: u64,
    end_ts: Option<u64>,
    tags: Vec<Tag>,
}

impl Track {
    fn interesting_tag(name: &str) -> bool {
        !name.ends_with("]") && name != "CUESHEET" && name != "LOG"
    }

    fn from_tags(cue: &Cue, next: Option<&&Cue>, tags: &[Tag]) -> Self {
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
        Self {
            number: cue.index,
            start_ts: cue.start_ts,
            end_ts: next.map(|cue| cue.start_ts),
            tags,
        }
    }
}
