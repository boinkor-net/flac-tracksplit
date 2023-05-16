use std::io::Write;

use anyhow::Context;
use int_conv::Truncate;
use symphonia_core::{
    checksum::{Crc16Ansi, Crc8Ccitt},
    formats::Packet,
    io::{Monitor, ReadBytes},
};
use tracing::debug;

#[derive(Default)]
pub struct OffsetFrame {
    initial_offset: Option<u64>,
}

impl OffsetFrame {
    /// Processes a FLAC frame by rewriting its sample/frame offset
    /// and CRC checksums, and emits that frame in an updated byte
    /// buffer.
    ///
    /// Returns a byte buffer containing the updated frame and boolean
    /// flags indicating whether the header and footer CRCs match what
    /// was there before.
    pub fn process(&mut self, packet: Packet) -> anyhow::Result<(Vec<u8>, bool, bool)> {
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
        match block_size_enc & 0b1111 {
            0b0110 => {
                // block size is given in the next 8 bits:
                let bs_u8 = frame_reader.read_u8().context("8bit block size")?;
                header_crc.process_byte(bs_u8);
                footer_crc.process_byte(bs_u8);
                frame_out.write_all(&[bs_u8])?;
            }
            0b0111 => {
                // block size given in the next 16 bits:
                let bs = frame_reader.read_be_u16().context("8bit block size")?;
                let bs_u8 = bs.to_be_bytes();
                header_crc.process_double_bytes(bs_u8);
                footer_crc.process_double_bytes(bs_u8);
                frame_out.write_all(&bs_u8)?;
            }
            _ => {
                // No bits used for the field otherwise
            }
        }
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
        let original_header_crc = frame_reader.read_u8().context("reading header CRC")?;
        let my_header_crc = header_crc.crc();
        footer_crc.process_byte(my_header_crc);
        frame_out.write_all(&[my_header_crc])?;

        // Next, the subframes; we do not touch them, but we do rewrite the footer CRC:
        let remainder = frame_reader.read_buf_bytes_available_ref();
        let subframes = &remainder[..remainder.len() - 2];
        let original_footer_crc_u8 = &remainder[remainder.len() - 2..];
        let original_footer_crc = u16::from_be_bytes(original_footer_crc_u8.try_into().unwrap());
        footer_crc.process_buf_bytes(subframes);
        frame_out.write_all(subframes)?;

        let my_footer_crc = footer_crc.crc();
        let my_footer_crc_u8 = my_footer_crc.to_be_bytes();
        frame_out.write_all(&my_footer_crc_u8)?;

        Ok((
            frame_out,
            original_header_crc == my_header_crc,
            original_footer_crc == my_footer_crc,
        ))
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
    use std::fmt;
    use symphonia_core::io::BufReader;
    use tracing::info;

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
            info!(input, "yay");
        }
    }
}
