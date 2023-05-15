use anyhow::Context;
use crc::{Algorithm, Crc};
use int_conv::Truncate;
use metaflac::{
    block::{Picture, PictureType, StreamInfo, VorbisComment},
    Block,
};
use more_asserts as ma;
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
    checksum::Crc8Ccitt,
    formats::{Cue, FormatReader},
    io::{MediaSourceStream, Monitor},
    meta::{Tag, Value, Visual},
};
use tracing::{debug, info, instrument};
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
                total_samples: (end_ts - cue.start_ts),
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

    #[instrument(skip(self, to), fields(number = self.number, path = ?self.pathname()), err)]
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

    #[instrument(skip(self, from, to), fields(number = self.number, path = ?self.pathname()), err)]
    fn write_audio<S: Write>(&self, from: &mut FlacReader, mut to: S) -> anyhow::Result<()> {
        // TODO: Maybe seek. Currently, this is only called in sequence, so no need to do that.
        let mut last_end: u64 = 0;
        let mut first_sample_offset: Option<u64> = None;
        loop {
            // TODO: the "end" logic below is wrong.
            let packet = from
                .next_packet()
                .with_context(|| format!("last end: {:?} vs {:?}", last_end, self.end_ts))?;

            // fixup sample numbers: First, we read the initial sample
            // offset; then we replace it with a number that's rooted
            // in 0.
            let mut buf = symphonia_core::io::BufReader::new(&packet.buf()[4..]);
            let (orig_sample_offset, _) = utf8_decode_be_u64(&mut buf)?;
            if first_sample_offset.is_none() {
                info!(orig_sample_offset, "first sample offset");
                first_sample_offset.replace(orig_sample_offset);
            }
            let sample_offset = orig_sample_offset - first_sample_offset.unwrap();

            let ts = packet.ts;
            ma::assert_ge!(
                ts,
                self.start_ts,
                "Packet timestamp is not >= this track's start ts. Potential bug exposed by the previous track.",
            );
            // let old_checksum = packet.buf()[packet.buf().len() - 1];
            // let new_checksum = crc.checksum(&packet.buf()[..(packet.buf().len() - 2)]);
            // debug_assert_eq!(
            //     old_checksum, new_checksum,
            //     "crc old: 0x{:x} new: 0x{:x}, sample offset: {}",
            //     old_checksum, new_checksum, sample_offset
            // );

            let mut updated_buf = packet.buf()[0..4].to_owned();
            updated_buf.extend(utf8_encode_be_u64(sample_offset)?);
            updated_buf.extend(buf.read_buf_bytes_available_ref()); // remaining bytes after sample offset

            if Some(orig_sample_offset) == first_sample_offset {
                info!(
                    theirs = ?&packet.buf()[4..14],
                    mine = ?&updated_buf[4..14],
                    "sample offset matches"
                );
            }

            // let crc_offset = updated_buf.len() - 1;
            // let old_checksum = updated_buf[crc_offset];
            // updated_buf[crc_offset] = crc.checksum(&updated_buf[..crc_offset]);
            // info!(
            //     "crc old: {:x} new: {:x}",
            //     old_checksum, updated_buf[crc_offset]
            // );

            to.write_all(&updated_buf)?;

            last_end = ts + packet.dur;
            if last_end >= self.end_ts {
                return Ok(());
            }
        }
    }
}

const CRC_8_FLAC: Algorithm<u8> = Algorithm {
    poly: 0x17,
    init: 0x0,
    refin: false,
    refout: false,
    xorout: 0x0,
    check: 0x0,
    residue: 0x0,
};

/// Decodes a big-endian unsigned integer encoded via extended UTF8. In this context, extended UTF8
/// simply means the encoded UTF8 value may be up to 7 bytes for a maximum integer bit width of
/// 36-bits. Returns the number of bytes that contain the encoded number as the second value.
///
// Taken from symphonia.
fn utf8_decode_be_u64<B: symphonia_core::io::ReadBytes>(src: &mut B) -> anyhow::Result<(u64, u32)> {
    // Read the first byte of the UTF8 encoded integer.
    let mut state = u64::from(src.read_u8()?);

    // UTF8 prefixes 1s followed by a 0 to indicate the total number of bytes within the multi-byte
    // sequence. Using ranges, determine the mask that will overlap the data bits within the first
    // byte of the sequence. For values 0-128, return the value immediately. If the value falls out
    // of range return None as this is either not the start of a UTF8 sequence or the prefix is
    // incorrect.
    let mask: u8 = match state {
        0x00..=0x7f => return Ok((state, 1)),
        0xc0..=0xdf => 0x1f,
        0xe0..=0xef => 0x0f,
        0xf0..=0xf7 => 0x07,
        0xf8..=0xfb => 0x03,
        0xfc..=0xfd => 0x01,
        0xfe => 0x00,
        _ => anyhow::bail!("invalid utf-8 encoded sample/frame number"),
    };

    // Obtain the data bits from the first byte by using the data mask.
    state &= u64::from(mask);

    // Read the remaining bytes within the UTF8 sequence. Since the mask 0s out the UTF8 prefix
    // of 1s which indicate the length of the multi-byte sequence in bytes, plus an additional 0
    // bit, the number of remaining bytes to read is the number of zeros in the mask minus 2.
    // To avoid extra computation, simply loop from 2 to the number of zeros.
    for _i in 2..mask.leading_zeros() {
        // Each subsequent byte after the first in UTF8 is prefixed with 0b10xx_xxxx, therefore
        // only 6 bits are useful. Append these six bits to the result by shifting the result left
        // by 6 bit positions, and appending the next subsequent byte with the first two high-order
        // bits masked out.
        state = (state << 6) | u64::from(src.read_u8()? & 0x3f);

        // TODO: Validation? Invalid if the byte is greater than 0x3f.
    }

    Ok((state, mask.leading_zeros() - 1))
}

fn utf8_encode_be_u64(input: u64) -> anyhow::Result<Vec<u8>> {
    let mut number = input;
    let start_mask: u8 = match number.leading_zeros() {
        57..=64 => return Ok(vec![number.truncate()]),
        53..=56 => 0b1100_0000,
        48..=52 => 0b1110_0000,
        44..=47 => 0b1111_0000,
        39..=43 => 0b1111_1000,
        34..=38 => 0b1111_1100,
        29..=33 => 0b1111_1110,
        00..=28 | 65..=u32::MAX => unreachable!("can't encode more than 7 leading bits"),
    };
    debug_assert!(
        64 - input.leading_zeros()
            <= (start_mask.count_ones() - 1) * 6 + (start_mask.count_zeros() - 1),
        "Number with {} set bits ({:b}) to be represented in {} bits",
        64 - input.leading_zeros(),
        input,
        (start_mask.count_ones() - 1) * 6 + (start_mask.count_zeros() - 1)
    );
    let len: usize = start_mask.count_ones() as usize;
    debug!(
        len,
        start_mask,
        leading_zeros = input.leading_zeros(),
        "allocating a vec for mask",
    );
    let mut val = vec![0; len];
    let mut posn = len - 1;
    while posn > 0 {
        let byte: u8 = number.truncate();
        val[posn] = (byte & 0x3f) | 0b10000000;
        number >>= 6;
        posn -= 1;
    }
    let byte: u8 = number.truncate();
    val[0] = byte | start_mask;
    debug_assert_eq!(
        0,
        byte >> (start_mask.count_zeros() - 1) as usize,
        "Should have 0 set bits left"
    );
    Ok(val)
}

#[cfg(test)]
mod test {
    use super::*;
    use std::fmt;
    use symphonia_core::io::BufReader;

    struct V<'a>(&'a [u8]);

    impl<'a> fmt::Binary for V<'a> {
        fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
            // extract the value using tuple idexing
            // and create reference to 'vec'
            let vec = &self.0;

            // @count -> the index of the value,
            // @n     -> the value
            for (count, n) in vec.iter().enumerate() {
                if count != 0 {
                    write!(f, " ")?;
                }

                write!(f, "{:b}", n)?;
            }

            Ok(())
        }
    }

    #[test]
    fn test_encoding() {
        let inputs: &[u64] = &[0x85, 0x863, 0x18427, 0xf88204, 0x04, 8790];
        for input in inputs {
            let encoded = utf8_encode_be_u64(*input).expect("encoding");
            let mut buf = BufReader::new(&encoded);
            let decoded =
                utf8_decode_be_u64(&mut buf).expect(&format!("decoding {:b}", V(&encoded)));
            assert_eq!(
                (*input, encoded.len() as u32),
                decoded,
                "received:\n{:#064b} but wanted:\n{:#064b}",
                decoded.0,
                input
            );
            info!("yay: {}", input);
        }
    }
}
