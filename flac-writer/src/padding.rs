use std::io;

use itertools::Itertools;

const PADDING_CHUNK_SIZE: usize = 20 * 1024;

pub(crate) fn write_padding<S: io::Write>(to: &mut S, n_bytes: u32) -> Result<(), io::Error> {
    let mut v = Vec::with_capacity(PADDING_CHUNK_SIZE);
    for chunk in &(0..n_bytes).chunks(PADDING_CHUNK_SIZE) {
        v.resize(chunk.count(), 0);
        to.write_all(&v)?;
    }
    Ok(())
}
