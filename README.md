![Build Status](https://github.com/antifuchs/flac-tracksplit/actions/workflows/ci.yml/badge.svg) [![Docs](https://docs.rs/flac-tracksplit/badge.svg)](https://docs.rs/flac-tracksplit/) [![crates.io](https://img.shields.io/crates/v/flac-tracksplit.svg)](https://crates.io/crates/flac-tracksplit)

# `flac-tracksplit` - a tool for splitting whole-disc FLAC files with embedded CUE sheets up into multiple tracks

Say you were impressed by the conceptual purity of representing a *whole CD* as a single losslessly-compressed FLAC file, for archival purposes, say 11 years ago. You sunk a ton of time into ripping these CDs, dedicated a ton of storage space to them and also ensured that all the CDs you have are accurately represented as MusicBrainz releases and have perfect metadata, and all that.

And further say that now you want to ... actually use these files for things, whereupon you discover that next to nothing supports the CUE+FLAC format anymore, and the only way to really get anything useful out of the hundreds of gigabytes of data you now have is to re-encode and re-tag and re-sort them. So, what do you do?

You could turn to tools like [unflac](https://sr.ht/~ft/unflac/) or [trackfs](https://github.com/andresch/trackfs), but you'll find that they lose bits of metadata that might be present on your existing archival copies, and that they go and decode the FLAC file only to re-encode it again - what a waste of CPU time! (And in the case of trackfs, what a waste of wallclock time too - re-encoding is far too slow to stream the tracks with [navidrome](https://www.navidrome.org/), say).

So maybe this tool can help.

`flac-tracksplit` does frame-accurate FLAC splitting along track boundaries, with a focus on *not doing unnecessary work*, and especially not re-encoding all that valuable data. It commits various crimes to get a split-out set of tracks from your archival copies, but those tracks do contain all the per-track (and whole-album) tags you have set on them, as well as decode correctly (with seeking), and they all start and end on the correct time stamps (caveat, they end on the `FRAME` boundary, which may include a few samples from the next track; this is not more than a few milliseconds in typical use though).

To use it, run `flac-tracksplit --base-path /output/files/will/go/here /path/to/your/archival/copies/*.flac`

The splitting process is multi-threaded (one archival file being processed per physical core in your machine) and should take no more than about a second per album.

## Future Work

This tool is fast enough to make a FUSE filesystem viable! Maybe somebody wants to contribute one of those, that would be hilarious. (But then, I am using this tool to split my cue+flac files out into per-track copies and will start treating those as the canonical data set; too few things out there support cue+flac to make these archival copies worth it.)
