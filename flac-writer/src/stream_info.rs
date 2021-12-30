//! Extensions for writing FLAC `StreamInfo` blocks to streams.

use std::io::{self, Write};

use byteorder::{BigEndian, WriteBytesExt};
use int_conv::Truncate;
use symphonia_utils_xiph::flac::metadata::StreamInfo;

/// Extension trait for writing a [`StreamInfo`] extension trait to a
/// stream.
pub trait StreamInfoWriteExt: Sized {
    /// Override the MD5 sum of the stream
    fn with_md5(self, md5sum: [u8; 16]) -> Self;

    /// Set the StreamInfo block's MD5 to the "unknown" value.
    fn without_md5(self) -> Self {
        self.with_md5([0u8; 16])
    }

    /// Override the number of samples in the stream.
    fn with_samples(self, samples: Option<u64>) -> Self;

    /// Set the StreamInfo block's sample count to the "unknown" value.
    fn without_samples(self) -> Self {
        self.with_samples(None)
    }
}

impl StreamInfoWriteExt for StreamInfo {
    fn with_md5(mut self, md5sum: [u8; 16]) -> Self {
        self.md5 = md5sum;
        self
    }

    fn with_samples(mut self, samples: Option<u64>) -> Self {
        self.n_samples = samples;
        self
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WriteStreamInfoError {
    #[error("couldn't write STREAMINFO block header")]
    Header(#[from] crate::WriteMetadataBlockHeaderError),

    #[error("IO error writing STREAMINFO")]
    Io(#[from] io::Error),
}

pub(crate) fn write_streaminfo<S: Write>(
    to: &mut S,
    info: &StreamInfo,
) -> Result<(), WriteStreamInfoError> {
    to.write_u16::<BigEndian>(info.block_len_min)?;
    to.write_u16::<BigEndian>(info.block_len_max)?;
    to.write_u24::<BigEndian>(info.frame_byte_len_min)?;
    to.write_u24::<BigEndian>(info.frame_byte_len_max)?;

    to.write_u16::<BigEndian>((info.sample_rate >> 4).truncate())?;

    // Next byte contains:
    // 4 bits for the rest of the sample rate,
    // 3 bits for the number of channels,
    // 1 bit for the most significant bit of the bits per sample (minus one).
    let sample_rate_ls_nybble: u8 = (info.sample_rate & 0b1111).truncate();

    let num_channels: u8 = (info.channels.bits() - 1).truncate();
    let bits_per_sample: u8 = (info.bits_per_sample - 1).truncate();
    let bps_msb = bits_per_sample >> 4;
    to.write_u8((sample_rate_ls_nybble) << 5 | (num_channels << 1) | bps_msb)?;

    // Next byte contains:
    // 4 least significant bits of the bps
    // 4 most significant bits of the total sample count.
    let bps_lsb = bits_per_sample & 0b1111;
    let samples = info.n_samples.unwrap_or(0);
    let n_samples_msb: u8 = (samples >> 32).truncate();
    to.write_u8(bps_lsb << 4 | n_samples_msb)?;

    to.write_u32::<BigEndian>((samples & 0xFFFF_FFFF).truncate())?;

    to.write_all(&info.md5)?;
    Ok(())
}
