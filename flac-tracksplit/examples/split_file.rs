use std::fs::{create_dir_all, File};

use flac_tracksplit::{Track, LEAD_OUT_TRACK_NUMBER};
use metaflac::block::StreamInfo;
use symphonia_bundle_flac::FlacReader;
use symphonia_core::{
    formats::{Cue, FormatReader},
    io::MediaSourceStream,
};
use tracing::{debug, info};
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

fn main() {
    // Setup logging:
    let indicatif_layer = tracing_indicatif::IndicatifLayer::new();
    let filter = EnvFilter::builder()
        .with_default_directive(tracing_subscriber::filter::LevelFilter::INFO.into())
        .from_env_lossy();
    let writer = indicatif_layer.get_stderr_writer();
    let app_log_layer = tracing_subscriber::fmt::layer()
        .with_target(false)
        .compact()
        .with_writer(writer.clone());
    tracing_subscriber::registry()
        .with(filter)
        .with(app_log_layer)
        .with(indicatif_layer)
        .init();

    let file = File::open("test.flac").expect("opening test.flac");
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut reader = FlacReader::try_new(mss, &Default::default()).expect("creating flac reader");
    debug!("tracks: {:?}", reader.tracks());
    let track = reader.default_track().expect("default track");
    let data = match &track.codec_params.extra_data {
        Some(it) => it,
        _ => return,
    };
    let info = StreamInfo::from_bytes(data);
    let cues: Vec<Cue> = reader.cues().iter().cloned().collect();
    let time_base = track.codec_params.time_base.expect("time base");
    assert_eq!(time_base.numer, 1, "Should be a fraction like 1/44000");
    assert_eq!(
        time_base.denom, info.sample_rate,
        "Should have the sample rate as denom"
    );
    // since we're sure that the sample rate is an even denominator of
    // symphonia's TimeBase, we can assume that the time stamps are in
    // samples:
    let last_ts: u64 = info.total_samples;

    let mut cue_iter = cues.iter().peekable();
    while let Some(cue) = cue_iter.next() {
        let next = cue_iter.peek();
        let end_ts = match next {
            None => last_ts, // no lead-out, fudge it.
            Some(track) if track.index == LEAD_OUT_TRACK_NUMBER => {
                // we have a lead-out, capture the whole in the last track.
                let end_ts = track.start_ts;
                cue_iter.next();
                end_ts
            }
            Some(track) => track.start_ts,
        };
        let track = {
            let metadata = reader.metadata();
            let current_metadata = metadata.current().expect("tags");
            let tags = current_metadata.tags();
            let visuals = current_metadata.visuals();
            Track::from_tags(&info, cue, end_ts, &tags, &visuals)
        };
        info!(number = track.number, pathname = ?track.pathname(), start_ts = track.start_ts, end_ts = track.end_ts);
        let path = track.pathname();
        if let Some(parent) = path.parent() {
            create_dir_all(parent).expect("creating album dir");
        }
        let mut f = File::create(track.pathname()).unwrap();
        track
            .write_metadata(&mut f)
            .expect(&format!("writing track {:?}", track.pathname()));
        track
            .write_audio(&mut reader, &mut f)
            .expect("writing track");
    }
}
