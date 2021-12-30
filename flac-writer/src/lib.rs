use byteorder::{BigEndian, WriteBytesExt};
use padding::write_padding;
use std::io::{self, Write};
use stream_info::{write_streaminfo, WriteStreamInfoError};

use symphonia_core::meta::{Tag, Visual};
use symphonia_utils_xiph::flac::metadata::StreamInfo;

pub mod padding;
pub mod stream_info;
pub use stream_info::StreamInfoWriteExt;

const FLAC_STREAM_MARKER: &[u8; 4] = b"fLaC";

const STREAMINFO_BYTE_LENGTH: u32 = 34;

pub enum MetadataBlock<'a> {
    StreamInfo(&'a StreamInfo),
    Padding { length: u32 },
    Application { id: u32, data: &'a [u8] },
    SeekTable, // TODO
    VorbisComment { tags: &'a [Tag] },
    CueSheet,
    Picture { picture: &'a Visual },
    Reserved,
}

/// Errors that can occur during metadata-block writing.
#[derive(Debug, thiserror::Error)]
pub enum WriteMetadataBlockHeaderError {
    #[error("An unknown FLAC metadata header type was encountered")]
    UnknownType,

    #[error("IO error writing FLAC metadata header block")]
    Io(#[from] io::Error),
}

fn write_metadata_block_header<S: Write>(
    to: &mut S,
    is_last: bool,
    block: &MetadataBlock,
) -> Result<(), WriteMetadataBlockHeaderError> {
    use MetadataBlock::*;
    let (block_type, byte_length) = match block {
        StreamInfo(_) => (0, STREAMINFO_BYTE_LENGTH),
        Padding { length } => (1, *length),
        // Application { .. } => 2,
        // SeekTable { .. } => 3,
        // VorbisComment(_) => 4,
        // CueSheet => 5,
        // Picture => 6,
        _ => return Err(WriteMetadataBlockHeaderError::UnknownType),
    };

    // 31: is last
    // 30..24: type
    // 24..0: length of data to follow.
    let header = block_type | (is_last as u8) << 7;
    to.write_u8(header)?;
    to.write_u24::<BigEndian>(byte_length)?;
    Ok(())
}

/// Errors that `write_flac_stream_header` can return.
#[derive(Debug, thiserror::Error)]
pub enum WriteFlacStreamError {
    #[error("couldn't write header")]
    Header(#[from] WriteMetadataBlockHeaderError),

    #[error("couldn't write STREAMINFO block")]
    StreamInfo(#[from] WriteStreamInfoError),

    #[error("IO error writing initial metadata blocks")]
    Io(#[from] io::Error),
}

/// Write header data that identifies a valid FLAC stream.
///
/// First, the bytes `fLaC`, then the StreamInfo metadata block,
/// followed by an optional set of additional metadata blocks.
pub fn write_flac_stream_header<S: Write>(
    to: &mut S,
    info: &StreamInfo,
    blocks: &[&MetadataBlock],
) -> Result<(), WriteFlacStreamError> {
    to.write_all(FLAC_STREAM_MARKER)?;
    let streaminfo_is_last = blocks.is_empty();
    write_metadata_block_header(to, streaminfo_is_last, &MetadataBlock::StreamInfo(info))?;
    write_streaminfo(to, info)?;
    let mut block_iter = blocks.iter().peekable();
    while let Some(block) = block_iter.next() {
        write_metadata_block_header(to, block_iter.peek().is_none(), block)?;
        match block {
            MetadataBlock::StreamInfo(info) => write_streaminfo(to, info)?,
            MetadataBlock::Padding { length } => write_padding(to, *length)?,
            MetadataBlock::Application { .. } => todo!(),
            MetadataBlock::SeekTable => todo!(),
            MetadataBlock::VorbisComment { .. } => todo!(),
            MetadataBlock::CueSheet => todo!(),
            MetadataBlock::Picture { .. } => todo!(),
            MetadataBlock::Reserved => todo!(),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
