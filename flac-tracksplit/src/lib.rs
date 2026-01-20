use anyhow::{bail, Context};
use int_conv::Truncate;
use metaflac::{
    block::{Picture, PictureType, StreamInfo, VorbisComment},
    Block,
};
use more_asserts as ma;
use std::{
    borrow::Cow,
    fmt::Debug,
    fs::{create_dir_all, File},
    io::Write,
    num::NonZeroU32,
    path::{Path, PathBuf},
    str::FromStr,
};
use symphonia_bundle_flac::FlacReader;
use symphonia_core::{
    checksum::{Crc16Ansi, Crc8Ccitt},
    formats::{Cue, FormatReader, Packet},
    io::{MediaSourceStream, Monitor, ReadBytes},
    meta::{StandardVisualKey, Tag, Value, Visual},
};
use tracing::{debug, info, instrument, warn};

#[instrument(skip(base_path, metadata_padding), err)]
pub fn split_one_file<P: AsRef<Path> + Debug, B: AsRef<Path> + Debug>(
    input_path: P,
    base_path: B,
    metadata_padding: u32,
) -> anyhow::Result<Vec<PathBuf>> {
    let file = File::open(&input_path).with_context(|| format!("opening {:?}", input_path))?;
    let file_length = file.metadata().context("file metadata")?.len();
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut reader =
        FlacReader::try_new(mss, &Default::default()).context("could not create flac reader")?;
    debug!("tracks: {:?}", reader.tracks());
    let track = reader.default_track().context("no default track")?;
    let data = match &track.codec_params.extra_data {
        Some(it) => it,
        _ => bail!("Unclear track codec params - Not a flac file?"),
    };
    let info = StreamInfo::from_bytes(data);
    let cues = reader.cues().to_vec();
    let time_base = track.codec_params.time_base.context("track time base")?;
    if time_base.numer != 1 {
        bail!(
            "track time_base numerator should be a fraction like 1/44000, instead {:?}",
            time_base
        );
    }
    if time_base.denom != info.sample_rate {
        bail!("track time_base denominator ({:?}) should be the same as the overall streaminfo ({:?})", time_base, info.sample_rate);
    }
    // since we're sure that the sample rate is an even denominator of
    // symphonia's TimeBase, we can assume that the time stamps are in
    // samples:
    let last_ts: u64 = info.total_samples;

    let mut track_paths = vec![];
    let mut cue_iter = cues.iter().peekable();
    let mut audio_buffer = Vec::with_capacity(file_length.try_into().unwrap());
    if cue_iter.peek().is_none() {
        warn!(
            action="skipping",
            remedy="Use `metaflac --import-cuesheet-from` to add the sheet and make the file splittable.",
            "No embedded CUE sheet found."
        );
        return Ok(track_paths);
    }
    while let Some(cue) = cue_iter.next() {
        audio_buffer.clear();
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
            let current_metadata = metadata.current().context("track tags")?;
            let tags = current_metadata.tags();
            let visuals = current_metadata.visuals();
            Track::from_tags(&info, cue, end_ts, tags, visuals)
        };
        debug!(number = track.number, output = ?track.pathname(), "Track");
        let pathbuf = base_path.as_ref().join(track.pathname());
        let path = &pathbuf;
        if let Some(parent) = path.parent() {
            create_dir_all(parent).context("creating album dir")?;
        }
        let mut f = File::create(path).unwrap();
        let sample_count = track
            .write_audio(&mut reader, &mut audio_buffer)
            .with_context(|| format!("buffering track {:?} audio", path))?;

        track
            .write_metadata(sample_count, metadata_padding, &mut f)
            .with_context(|| format!("writing track {:?}", path))?;
        f.write_all(&audio_buffer)
            .with_context(|| format!("writing track {:?} audio", path))?;
        track_paths.push(pathbuf);
    }
    info!("Done with disc image");
    Ok(track_paths)
}

/// The track number used to identify a lead-out track on a cue sheet.
pub const LEAD_OUT_TRACK_NUMBER: u32 = 170;

/// Metadata identifying a track in a FLAC file that has an embedded CUE sheet.
#[derive(Clone)]
pub struct Track {
    streaminfo: StreamInfo,
    pub number: u32,
    pub start_ts: u64,
    pub end_ts: u64,
    pub tags: Vec<Tag>,
    pub visuals: Vec<Visual>,
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
        !name.ends_with(']') && name != "CUESHEET" && name != "LOG"
    }

    /// Create a [Track] from a file's embedded FLAC&vorbis comments and CUE sheet.
    pub fn from_tags(
        streaminfo: &StreamInfo,
        cue: &Cue,
        end_ts: u64,
        tags: &[Tag],
        visuals: &[Visual],
    ) -> Self {
        let suffix = format!("[{}]", cue.index);
        let tags = tags
            .iter()
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
        let visuals = visuals.to_vec();
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

    /// Return the tag value for a given tag name.
    pub fn tag_value(&self, name: &str) -> Option<&Value> {
        self.tags
            .iter()
            .find(|tag| tag.key == name)
            .map(|found| &found.value)
    }

    fn is_risky_char(c: char) -> bool {
        match c {
            // question marks, single quotes, forward / backslashes
            // and colons, shell escapey things are not safe; Picard
            // chokes on them sadly.
            ' ' | '_' | '-' | ',' | '.' | '!' | '&' | '(' | ')' | '[' | ']' | '{' | '}' | '<'
            | '>' => false,
            _ => !c.is_alphanumeric(),
        }
    }

    fn sanitize_pathname(name: &'_ str) -> Cow<'_, str> {
        if name.contains(Self::is_risky_char) {
            Cow::Owned(name.replace(Self::is_risky_char, "_"))
        } else {
            Cow::Borrowed(name)
        }
    }

    /// Return the output pathname for a track.
    pub fn pathname(&self) -> PathBuf {
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

        let disc_prefix = match (self.tag_value("DISCNUMBER"), self.tag_value("TOTALDISCS")) {
            (Some(Value::String(disc)), Some(Value::String(disc_total)))
                if usize::from_str(disc_total).map(|total| total > 1) == Ok(true) =>
            {
                if let Ok(discno) = usize::from_str(disc) {
                    Some(format!("{:02}-", discno))
                } else {
                    Some(format!("{}-", disc))
                }
            }
            _ => None,
        };
        if let (Some(Value::String(track)), Some(Value::String(title))) =
            (self.tag_value("TRACKNUMBER"), self.tag_value("TITLE"))
        {
            if let Ok(trackno) = <usize as FromStr>::from_str(track) {
                buf.push(format!(
                    "{}{:02}.{}.flac",
                    disc_prefix.as_deref().unwrap_or(""),
                    trackno,
                    Self::sanitize_pathname(title)
                ));
            } else {
                buf.push(format!(
                    "{}99.{}.flac",
                    disc_prefix.as_deref().unwrap_or(""),
                    Self::sanitize_pathname(title)
                ));
            }
        }
        buf
    }

    /// Write a track's
    /// [STREAM](https://xiph.org/flac/format.html#stream) metadata
    /// blocks - first STREAMINFO, then the remainder containing
    /// pictures and vorbis comments.
    #[instrument(skip(self, to), fields(number = self.number, path = ?self.pathname()), err)]
    pub fn write_metadata<S: Write>(
        &self,
        total_samples: u64,
        metadata_padding: u32,
        mut to: S,
    ) -> anyhow::Result<()> {
        to.write_all(b"fLaC")?;
        let comment = VorbisComment {
            vendor_string: "flac-tracksplit".to_string(),
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
                    picture_type: translate_visual_key(
                        visual.usage.unwrap_or(StandardVisualKey::OtherIcon),
                    ),
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
        let mut streaminfo = self.streaminfo.clone();
        if total_samples != streaminfo.total_samples {
            // This is a pretty peaceful condition (difference is
            // about less than 1/10s), but let's let curious users
            // know since it's the one thing that is "imprecise" about
            // how this tool operates.
            debug!(
                inferred = streaminfo.total_samples,
                actual = total_samples,
                duration_diff_s = (streaminfo.total_samples as f32 - total_samples as f32)
                    / (streaminfo.sample_rate as f32),
                "inferred and actual total samples differ."
            );
        }
        streaminfo.total_samples = total_samples;
        let headers = vec![Block::StreamInfo(streaminfo), Block::VorbisComment(comment)];
        for block in headers.into_iter().chain(pictures.into_iter()) {
            block
                .write_to(false, &mut to)
                .with_context(|| format!("writing block {:?}", block))?;
        }
        Block::Padding(metadata_padding)
            .write_to(true, &mut to)
            .context("writing padding")?;
        Ok(())
    }

    /// Write a STREAM's
    /// [FRAME](https://xiph.org/flac/format.html#frame) sequence,
    /// containing compressed audio samples. Returns the number of samples actually processed.
    #[instrument(skip(self, from, to), fields(number = self.number, path = ?self.pathname()), err)]
    pub fn write_audio<S: Write>(&self, from: &mut FlacReader, mut to: S) -> anyhow::Result<u64> {
        // TODO: Seek to the track start. Currently, this is only
        // called in sequence (we're parallel per-file), so no need to
        // do that rn, but it would be nice!

        let mut last_end: u64 = 0;
        let mut frame = OffsetFrame::default();
        loop {
            let packet = from
                .next_packet()
                .with_context(|| format!("last end: {:?} vs {:?}", last_end, self.end_ts))?;

            let ts = packet.ts;
            let dur = packet.dur;
            ma::assert_ge!(
                ts,
                self.start_ts,
                "Packet timestamp is not >= this track's start ts. Potential bug exposed by the previous track.",
            );

            // Adjust the frame header:
            // * Adjust sample/frame number such that each track starts at frame/sample 0. This should fix seeking.
            // * Recompute the 8-bit header CRC
            // * Recompute the 16-bit footer CRC

            let updated_buf = frame
                .process(packet)
                .with_context(|| format!("processing frame at ts {}", ts))?;
            to.write_all(&updated_buf)?;

            last_end = ts + dur;
            if last_end >= self.end_ts {
                return Ok(frame.samples_processed);
            }
        }
    }
}

fn translate_visual_key(key: StandardVisualKey) -> PictureType {
    use PictureType::*;
    match key {
        StandardVisualKey::FileIcon => Icon,
        StandardVisualKey::OtherIcon => Other,
        StandardVisualKey::FrontCover => CoverFront,
        StandardVisualKey::BackCover => CoverBack,
        StandardVisualKey::Leaflet => Leaflet,
        StandardVisualKey::Media => Media,
        StandardVisualKey::LeadArtistPerformerSoloist => LeadArtist,
        StandardVisualKey::ArtistPerformer => Artist,
        StandardVisualKey::Conductor => Conductor,
        StandardVisualKey::BandOrchestra => Band,
        StandardVisualKey::Composer => Composer,
        StandardVisualKey::Lyricist => Lyricist,
        StandardVisualKey::RecordingLocation => RecordingLocation,
        StandardVisualKey::RecordingSession => DuringRecording,
        StandardVisualKey::Performance => DuringPerformance,
        StandardVisualKey::ScreenCapture => ScreenCapture,
        StandardVisualKey::Illustration => Illustration,
        StandardVisualKey::BandArtistLogo => BandLogo,
        StandardVisualKey::PublisherStudioLogo => PublisherLogo,
    }
}

/// A FLAC stream's [Frame](https://xiph.org/flac/format.html#frame),
/// with samples that are offset such that the first frame has a
/// frame/sample offset of 0 and the others follow suit.
///
/// This is meant to be created once per [Track], and then updated for
/// all the frames making up that track.
#[derive(Default)]
pub struct OffsetFrame {
    initial_offset: Option<u64>,
    samples_processed: u64,
}

impl OffsetFrame {
    /// Processes a FLAC frame by rewriting its sample/frame offset
    /// and CRC checksums, and emits that frame in an updated byte
    /// buffer.
    ///
    /// Returns a byte buffer containing the updated frame.
    pub fn process(&mut self, packet: Packet) -> anyhow::Result<Vec<u8>> {
        let mut frame_reader = packet.as_buf_reader();
        let mut header_crc = Crc8Ccitt::new(0);
        let mut footer_crc = Crc16Ansi::new(0);
        let mut frame_out = Vec::with_capacity(packet.buf().len());

        // FLAC frame magic number / reserved bits
        let sync = frame_reader.read_be_u16().context("reading frame sync")?;
        let sync_u8 = sync.to_be_bytes();
        header_crc.process_double_bytes(sync_u8);
        footer_crc.process_double_bytes(sync_u8);
        frame_out.write_all(&sync_u8)?;

        // FLAC frame description
        let desc = frame_reader.read_be_u16().context("reading frame desc")?;
        let desc_u8 = desc.to_be_bytes();
        header_crc.process_double_bytes(desc_u8);
        footer_crc.process_double_bytes(desc_u8);
        frame_out.write_all(&desc_u8)?;
        let block_size_enc = u32::from((desc & 0xf000) >> 12);
        let sample_rate_enc = u32::from((desc & 0x0f00) >> 8);

        // Next up is the frame/sample number, here we munge some data:
        let (orig_sample_offset, _sample_n_bytes) =
            utf8_decode_be_u64(&mut frame_reader).context("decoding the sample offset")?;
        if self.initial_offset.is_none() {
            debug!(orig_sample_offset, "first sample offset");
            self.initial_offset.replace(orig_sample_offset);
        }
        let sample_offset = orig_sample_offset - self.initial_offset.unwrap();
        let offset_u8 = utf8_encode_be_u64(sample_offset).context("encoding the new offset")?;
        header_crc.process_buf_bytes(&offset_u8);
        footer_crc.process_buf_bytes(&offset_u8);
        frame_out.write_all(&offset_u8)?;

        // Now, some gymnastics to read the sample rate & block size
        // if necessary (behavior dictated by the appropriate bits in
        // the `desc` fields).
        let block_samples: u64 = match block_size_enc & 0b1111 {
            0b0110 => {
                // block size is given in the next 8 bits:
                let bs_u8 = frame_reader.read_u8().context("8bit block size")?;
                header_crc.process_byte(bs_u8);
                footer_crc.process_byte(bs_u8);
                frame_out.write_all(&[bs_u8])?;
                bs_u8.into()
            }
            0b0111 => {
                // block size given in the next 16 bits:
                let bs = frame_reader.read_be_u16().context("8bit block size")?;
                let bs_u8 = bs.to_be_bytes();
                header_crc.process_double_bytes(bs_u8);
                footer_crc.process_double_bytes(bs_u8);
                frame_out.write_all(&bs_u8)?;
                bs.into()
            }
            0b0001 => 192,
            0b0000 => bail!("reserved sample count"),
            b if b & 0b1000 != 0 => 256 * 2u64.pow((b & 0b1111) - 8),
            b => 576 * 2u64.pow((b & 0b111) - 2),
        };
        match sample_rate_enc & 0b1111 {
            0b1100 => {
                // sample rate is given in the next 8 bits:
                let sr_u8 = frame_reader.read_u8().context("8bit block size")?;
                header_crc.process_byte(sr_u8);
                footer_crc.process_byte(sr_u8);
                frame_out.write_all(&[sr_u8])?;
            }
            0b1101 | 0b1110 => {
                // sample rate given in the next 16 bits:
                let bs = frame_reader.read_be_u16().context("8bit block size")?;
                let sr_u8 = bs.to_be_bytes();
                header_crc.process_double_bytes(sr_u8);
                footer_crc.process_double_bytes(sr_u8);
                frame_out.write_all(&sr_u8)?;
            }
            0b1111 => anyhow::bail!("invalid sample rate: sync-fooling string of 1s"),
            _ => {
                // No bits used for the field otherwise
            }
        }

        // What follows is the header CRC. We write out the one computed above:
        let _original_header_crc = frame_reader.read_u8().context("reading header CRC")?;
        let my_header_crc = header_crc.crc();
        footer_crc.process_byte(my_header_crc);
        frame_out.write_all(&[my_header_crc])?;

        // Next, the subframes; we do not touch them, but we do rewrite the footer CRC:
        let remainder = frame_reader.read_buf_bytes_available_ref();
        let subframes = &remainder[..remainder.len() - 2];
        footer_crc.process_buf_bytes(subframes);
        frame_out.write_all(subframes)?;

        let my_footer_crc = footer_crc.crc();
        let my_footer_crc_u8 = my_footer_crc.to_be_bytes();
        frame_out.write_all(&my_footer_crc_u8)?;
        self.samples_processed += block_samples;
        Ok(frame_out)
    }
}

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
    use proptest::{prop_assert_eq, proptest};
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

    proptest! {
        #[test]
        fn test_encoding(input in 0..(2u64.pow(35))) {
            let encoded = utf8_encode_be_u64(input).expect("encoding");
            let mut buf = BufReader::new(&encoded);
            let decoded = utf8_decode_be_u64(&mut buf).unwrap_or_else(|_| panic!("decoding {:b}", V(&encoded)));
            prop_assert_eq!((input, encoded.len() as u32), decoded,
                "received:\n{:#064b} but wanted:\n{:#064b}",
                decoded.0, input);
        }
    }
}
