use std::fs::File;

use symphonia_bundle_flac::FlacReader;
use symphonia_core::{formats::FormatReader, io::MediaSourceStream};
use symphonia_utils_xiph::flac::metadata::StreamInfo;

fn main() {
    let file = File::open("test.flac").expect("opening test.flac");
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let reader = FlacReader::try_new(mss, &Default::default()).expect("creating flac reader");
    // println!("tracks: {:?}", reader.tracks());
    if let Some(data) = &reader
        .default_track()
        .expect("default track")
        .codec_params
        .extra_data
    {
        let info = StreamInfo::read(&mut symphonia_core::io::BufReader::new(&data))
            .expect("parse STREAMINFO");
        println!("hex: {:?}", info);
    }
    println!("cues: {:?}", reader.cues());
}
