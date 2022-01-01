use anyhow::Context;
use metaflac::{
    block::{Picture, PictureType, StreamInfo, VorbisComment},
    Block,
};
use std::{
    borrow::Cow,
    fs::{create_dir_all, File},
    io::Write,
    num::NonZeroU32,
    path::{is_separator, PathBuf},
    str::FromStr,
};
use symphonia_bundle_flac::FlacReader;
use symphonia_core::{
    formats::{Cue, FormatReader},
    io::MediaSourceStream,
    meta::{Tag, Value, Visual},
};

fn main() {
    let file = File::open("test.flac").expect("opening test.flac");
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut reader = FlacReader::try_new(mss, &Default::default()).expect("creating flac reader");
    // println!("tracks: {:?}", reader.tracks());
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
            None => last_ts,
            Some(track) if track.index == LEAD_OUT_TRACK_NUMBER => {
                // we have a lead-out, capture the whole in the last track.
                cue_iter.next();
                last_ts
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
        println!(
            "\ntrack {}: {:?} = {:?}",
            track.number,
            track.pathname(),
            track
        );
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

const LEAD_OUT_TRACK_NUMBER: u32 = 170;

#[derive(Clone)]
struct Track {
    streaminfo: StreamInfo,
    number: u32,
    start_ts: u64,
    end_ts: u64,
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

    fn from_tags(
        streaminfo: &StreamInfo,
        cue: &Cue,
        end_ts: u64,
        tags: &[Tag],
        visuals: &[Visual],
    ) -> Self {
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
            streaminfo: StreamInfo {
                md5: [0u8; 16].to_vec(),
                total_samples: 0, // (end_ts - cue.start_ts)
                ..streaminfo.clone()
            },
            number: cue.index,
            start_ts: cue.start_ts,
            end_ts,
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

    fn sanitize_pathname(name: &str) -> Cow<str> {
        if name.contains(is_separator) {
            Cow::Owned(name.replace(is_separator, "_"))
        } else {
            Cow::Borrowed(name)
        }
    }

    fn pathname(&self) -> PathBuf {
        let mut buf = PathBuf::new();
        if let Some(Value::String(artist)) = self.tag_value("ALBUMARTIST") {
            buf.push(Self::sanitize_pathname(artist).as_ref());
        } else if let Some(Value::String(artist)) = self.tag_value("ARTIST") {
            buf.push(Self::sanitize_pathname(artist).as_ref());
        } else {
            buf.push("Unknown Artist");
        }

        if let Some(Value::String(album)) = self.tag_value("ALBUM") {
            if let Some(Value::String(year)) = self.tag_value("DATE") {
                buf.push(format!("{} - {}", year, Self::sanitize_pathname(album)));
            } else {
                buf.push(Self::sanitize_pathname(album).as_ref());
            }
        } else {
            buf.push("Unknown Album");
        }

        if let (Some(Value::String(track)), Some(Value::String(title))) =
            (self.tag_value("TRACKNUMBER"), self.tag_value("TITLE"))
        {
            if let Ok(trackno) = <usize as FromStr>::from_str(track) {
                buf.push(format!(
                    "{:02}.{}.flac",
                    trackno,
                    Self::sanitize_pathname(title)
                ));
            } else {
                buf.push(format!("99.{}.flac", Self::sanitize_pathname(title)));
            }
        }
        buf
    }

    fn write_metadata<S: Write>(&self, mut to: S) -> anyhow::Result<()> {
        to.write_all(b"fLaC")?;
        let comment = VorbisComment {
            vendor_string: "asf's silly track splitter".to_string(),
            comments: self
                .tags
                .iter()
                .map(|tag| (tag.key.to_string(), vec![tag.value.to_string()]))
                .collect(),
        };
        let pictures: Vec<Block> = self
            .visuals
            .iter()
            .map(|visual| {
                Block::Picture(Picture {
                    picture_type: PictureType::Other,
                    mime_type: visual.media_type.to_string(),
                    description: "".to_string(),
                    width: visual.dimensions.map(|s| s.width).unwrap_or(0),
                    height: visual.dimensions.map(|s| s.height).unwrap_or(0),
                    depth: visual.bits_per_pixel.map(NonZeroU32::get).unwrap_or(0),
                    num_colors: match visual.color_mode {
                        Some(symphonia_core::meta::ColorMode::Discrete) => 0,
                        Some(symphonia_core::meta::ColorMode::Indexed(n)) => n.get(),
                        None => 0,
                    },
                    data: visual.data.to_vec(),
                })
            })
            .collect();
        let headers = vec![
            Block::StreamInfo(self.streaminfo.clone()),
            Block::VorbisComment(comment),
        ];
        let mut blocks = headers.into_iter().chain(pictures.into_iter()).peekable();
        while let Some(block) = blocks.next() {
            block.write_to(blocks.peek().is_none(), &mut to)?;
        }
        Ok(())
    }

    fn write_audio<S: Write>(&self, from: &mut FlacReader, mut to: S) -> anyhow::Result<()> {
        // TODO: Maybe seek. Currently, this is only called in sequence, so no need to do that.
        let mut last_end: u64 = 0;
        loop {
            // TODO: the "end" logic below is wrong.
            let packet = from
                .next_packet()
                .with_context(|| format!("last end: {:?} vs {:?}", last_end, self.end_ts))?;
            let ts = packet.pts();
            assert!(
                ts >= self.start_ts,
                "Packet timestamp {:?} is not >= this track's start ts {:?}",
                ts,
                self.start_ts
            );
            to.write_all(packet.buf())?;
            last_end = ts + packet.duration();
            if ts + packet.duration() >= self.end_ts {
                return Ok(());
            }
        }
    }
}
