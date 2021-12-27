use claxon::{metadata::StreamInfo, FlacReader, FlacReaderOptions};

use super::*;

fn md5_checksum<const N: usize>(checksum: &str) -> [u8; N] {
    hex::decode(checksum)
        .expect("decoding MD5 checksum hex string")
        .try_into()
        .unwrap_or_else(|v: Vec<u8>| {
            panic!("Expected a Vec of length {} but it was {}", N, v.len())
        })
}

#[test]
fn simple_streaminfo() {
    let si = StreamInfo {
        min_block_size: 4608,
        max_block_size: 4608,
        min_frame_size: Some(0),
        max_frame_size: Some(19024),
        sample_rate: 44100,
        channels: 2,
        bits_per_sample: 16,
        samples: Some(118981800),
        md5sum: md5_checksum("2d19476b6abc3ef4e7c32b64110e59a5"),
    };
    let mut buf = Vec::new();
    write_flac_stream_header(&mut buf, &si, &[]).unwrap();

    assert_eq!(buf.len(), 4 + 4 + 34);

    let fr = FlacReader::new_ext(
        buf.as_slice(),
        FlacReaderOptions {
            metadata_only: true,
            read_vorbis_comment: false,
        },
    )
    .expect("read back the FLAC header");
    let si_back = fr.streaminfo();
    assert_eq!(si_back.md5sum, si.md5sum);
    assert_eq!(si_back.channels, si.channels);
}
