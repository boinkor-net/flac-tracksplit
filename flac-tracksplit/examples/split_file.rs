use std::fs::File;

use symphonia_bundle_flac::FlacReader;
use symphonia_core::{formats::FormatReader, io::MediaSourceStream, meta::Tag};
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
    println!("cues: {:?}", reader.cues());
    let metadata = reader.metadata();
    let tags = metadata.current().expect("tags").tags();
    // println!("meta: {:?}", tags);
    println!("track6: {:?}", Track::from_tags(6, &tags));
}

#[derive(Clone, Debug)]
struct Track {
    number: usize,
    tags: Vec<Tag>,
}

impl Track {
    fn interesting_tag(name: &str) -> bool {
        !name.ends_with("]") && name != "CUESHEET" && name != "LOG"
    }

    fn from_tags(number: usize, tags: &[Tag]) -> Self {
        let suffix = format!("[{}]", number);
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
        Self { number, tags }
    }
}
