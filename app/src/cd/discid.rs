//! MusicBrainz disc ID computation from a CD table of contents.
//!
//! Pure Rust — no `libdiscid` C dependency. The algorithm is documented at
//! <https://musicbrainz.org/doc/Disc_ID_Calculation>:
//!
//! 1. Build an uppercase-hex string from: first track number (2 digits),
//!    last track number (2 digits), the lead-out LBA (8 digits), then 99
//!    track-offset entries (8 digits each), padded with `"00000000"` when
//!    the disc has fewer than 99 tracks.
//! 2. SHA-1 the resulting ASCII string.
//! 3. Standard base64 of the digest, with the substitutions `+`→`.`,
//!    `/`→`_`, `=`→`-`.
//!
//! The output is what MusicBrainz keys on for disc-based release matching.

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use sha1::{Digest, Sha1};

/// A single audio track on a CD.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TocTrack {
    /// Track number as it appears on the disc (1-indexed).
    pub number: u8,
    /// Starting Logical Block Address of the track (absolute LBA, including
    /// the 150-frame pre-gap that audio CDs encode).
    pub offset_lba: u32,
}

/// A CD table of contents — the minimum set of fields needed to identify a
/// disc against MusicBrainz.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscToc {
    /// First track number on the disc. Almost always `1`.
    pub first_track: u8,
    /// Last track number on the disc.
    pub last_track: u8,
    /// LBA of the lead-out area (the address immediately following the last
    /// audio frame). Equivalent to the total runtime in frames + 150.
    pub lead_out_lba: u32,
    /// Track offsets in order. `tracks[i].number` should be `first_track + i`
    /// but this is not enforced — the encoder uses positional ordering.
    pub tracks: Vec<TocTrack>,
}

/// Maximum number of CD tracks the disc-ID algorithm reserves slots for.
///
/// The Red Book audio CD spec caps a disc at 99 tracks, matching this. A
/// `DiscToc` with more than 99 entries is malformed — `compute_disc_id` will
/// silently drop the excess and produce a stable-but-wrong hash, so we
/// `debug_assert!` rather than truncate silently. Release builds still
/// emit a (wrong) hash rather than panicking, which is the right trade-off
/// for an end-user TUI.
pub const MAX_TRACKS: usize = 99;

/// Compute the MusicBrainz disc ID for a TOC.
///
/// Returns the canonical 28-character base64 disc ID string (URL-safe with
/// `.`, `_`, `-` substitutions).
///
/// Panics in debug builds if `toc.tracks.len() > MAX_TRACKS` (the disc-ID
/// algorithm has no representation for that case). Release builds drop the
/// excess silently and emit a hash that will not match MusicBrainz.
pub fn compute_disc_id(toc: &DiscToc) -> String {
    debug_assert!(
        toc.tracks.len() <= MAX_TRACKS,
        "disc has {} tracks, exceeds Red Book cap of {MAX_TRACKS} — disc ID will be wrong",
        toc.tracks.len(),
    );

    let mut input = String::with_capacity(4 + 8 * (1 + MAX_TRACKS));
    input.push_str(&format!("{:02X}", toc.first_track));
    input.push_str(&format!("{:02X}", toc.last_track));
    input.push_str(&format!("{:08X}", toc.lead_out_lba));

    // 99 track offset slots. Real tracks first, zero-padded after.
    for i in 0..MAX_TRACKS {
        let offset = toc.tracks.get(i).map(|t| t.offset_lba).unwrap_or(0);
        input.push_str(&format!("{offset:08X}"));
    }

    let digest = Sha1::digest(input.as_bytes());
    let mut encoded = STANDARD.encode(digest);
    // MB's URL-safe-ish alphabet: not RFC 4648 URL-safe — it uses `.` for `+`.
    encoded = encoded
        .replace('+', ".")
        .replace('/', "_")
        .replace('=', "-");
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 15-track disc — exercises the multi-track path. Pinned value is
    /// cross-checked against a Python implementation of the algorithm
    /// described at <https://musicbrainz.org/doc/Disc_ID_Calculation>:
    ///
    /// ```python
    /// s = "%02X%02X" % (first, last) + "%08X" % lead_out
    /// for i in range(99): s += "%08X" % (offsets[i] if i < len(offsets) else 0)
    /// base64.b64encode(hashlib.sha1(s.encode()).digest()).decode() \
    ///   .replace("+",".").replace("/","_").replace("=","-")
    /// ```
    #[test]
    fn computes_fifteen_track_reference_vector() {
        let toc = DiscToc {
            first_track: 1,
            last_track: 15,
            lead_out_lba: 258725,
            tracks: vec![
                TocTrack {
                    number: 1,
                    offset_lba: 150,
                },
                TocTrack {
                    number: 2,
                    offset_lba: 17510,
                },
                TocTrack {
                    number: 3,
                    offset_lba: 33275,
                },
                TocTrack {
                    number: 4,
                    offset_lba: 45910,
                },
                TocTrack {
                    number: 5,
                    offset_lba: 57805,
                },
                TocTrack {
                    number: 6,
                    offset_lba: 78310,
                },
                TocTrack {
                    number: 7,
                    offset_lba: 94650,
                },
                TocTrack {
                    number: 8,
                    offset_lba: 109580,
                },
                TocTrack {
                    number: 9,
                    offset_lba: 132010,
                },
                TocTrack {
                    number: 10,
                    offset_lba: 149160,
                },
                TocTrack {
                    number: 11,
                    offset_lba: 165115,
                },
                TocTrack {
                    number: 12,
                    offset_lba: 177710,
                },
                TocTrack {
                    number: 13,
                    offset_lba: 203325,
                },
                TocTrack {
                    number: 14,
                    offset_lba: 215555,
                },
                TocTrack {
                    number: 15,
                    offset_lba: 235590,
                },
            ],
        };
        assert_eq!(compute_disc_id(&toc), "TqvKjMu7dMliSfmVEBtrL7sBSno-");
    }

    /// Single-track disc — exercises the zero-padding path. Pin value also
    /// cross-validated against the Python reference algorithm.
    #[test]
    fn computes_single_track_disc() {
        let toc = DiscToc {
            first_track: 1,
            last_track: 1,
            lead_out_lba: 200,
            tracks: vec![TocTrack {
                number: 1,
                offset_lba: 150,
            }],
        };
        assert_eq!(compute_disc_id(&toc), "4LyJlZog6n72cyDXYtIht7iun08-");
    }

    /// Empty `tracks` vec on a disc with a declared last track of 1 — exercise
    /// the defensive "missing track = offset 0" path. Not a valid real CD but
    /// the function should not panic.
    #[test]
    fn missing_tracks_zero_pad_safely() {
        let toc = DiscToc {
            first_track: 1,
            last_track: 1,
            lead_out_lba: 100_000,
            tracks: vec![],
        };
        let id = compute_disc_id(&toc);
        assert_eq!(id.len(), 28);
    }
}
