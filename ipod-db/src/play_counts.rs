//! Parser for the iPod's `Play Counts` sidecar file.
//!
//! The iPod firmware tracks plays and skips that happen between syncs in
//! `iPod_Control/iTunes/Play Counts`. iTunes (or any sync program) is
//! expected to read this file on connect, fold the per-track deltas into
//! the corresponding mhit entries in `iTunesDB`, and then delete the file.
//! The firmware regenerates an empty sidecar on next play.
//!
//! # File format (libgpod-documented)
//!
//! ```text
//! mhdp header (96 bytes):
//!   +0   "mhdp" magic
//!   +4   header_size (always 96)
//!   +8   entry_length (16, 20, 24, 28, or 32 — varies by iPod generation)
//!   +12  num_entries (u32)
//!   +16..96 zeros / unused
//!
//! per-entry (entry_length bytes — read whatever prefix fits):
//!   +0   play_count (u32)         — plays since last sync
//!   +4   last_played (u32)        — Mac HFS epoch
//!   +8   audiobook_speed (u32)    — usually 0; ignored
//!   +12  rating (u32)             — 0..=100, 0xFF = "unset"
//!   +16  bookmark_time (u32)      [if entry_length >= 20]
//!   +20  play_count_total (u32)   [if entry_length >= 24] — ignored, we use +0
//!   +24  skip_count (u32)         [if entry_length >= 28]
//!   +28  last_skipped (u32)       [if entry_length >= 32]
//! ```
//!
//! Entries are positional — the N-th entry in the file corresponds to the
//! N-th mhit in `iTunesDB`. There is no track ID in the file, so the parser
//! is only meaningful when paired with the iTunesDB it was written against.

use byteorder::{LittleEndian, ReadBytesExt};
use std::io::{Cursor, Read};

use crate::{IpodDbError, IpodTrack};

/// Sentinel value for "rating not set" in the Play Counts sidecar.
const RATING_UNSET: u32 = 0xFF;

/// One Play Counts entry. All fields are populated from whatever the entry
/// length supplied; absent fields are zero. `rating` is the raw 0..=100
/// scale (firmware uses 5-star × 20). `RATING_UNSET` (0xFF) means the
/// firmware did not change the rating since last sync.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlayCountEntry {
    pub play_count: u32,
    pub last_played: u32,
    pub rating: u32,
    pub skip_count: u32,
    pub last_skipped: u32,
}

/// Parse a `Play Counts` file's bytes into a vector of entries.
///
/// Returns an empty vector for an empty entry list, an error for invalid
/// magic / header / truncation. Per-entry buffer overruns are bounds-checked
/// — no panics on malformed input.
pub fn parse(bytes: &[u8]) -> Result<Vec<PlayCountEntry>, IpodDbError> {
    if bytes.len() < 16 {
        return Err(IpodDbError::Parse(format!(
            "Play Counts: header truncated ({} bytes)",
            bytes.len()
        )));
    }

    let mut cur = Cursor::new(bytes);
    let mut magic = [0u8; 4];
    cur.read_exact(&mut magic)?;
    if &magic != b"mhdp" {
        return Err(IpodDbError::Parse(format!(
            "Play Counts: bad magic {:?}",
            magic
        )));
    }

    let header_size = cur.read_u32::<LittleEndian>()?;
    let entry_length = cur.read_u32::<LittleEndian>()?;
    let num_entries = cur.read_u32::<LittleEndian>()?;

    if header_size < 16 || (header_size as usize) > bytes.len() {
        return Err(IpodDbError::Parse(format!(
            "Play Counts: invalid header_size {}",
            header_size
        )));
    }
    if entry_length < 16 {
        return Err(IpodDbError::Parse(format!(
            "Play Counts: entry_length {} too small (min 16)",
            entry_length
        )));
    }

    // Bound num_entries by remaining bytes so a hostile/corrupt header
    // can't trigger a giant Vec allocation. Mirrors the defensive cap in
    // proplist parsing.
    let remaining = bytes
        .len()
        .saturating_sub(header_size as usize)
        .saturating_div(entry_length as usize);
    let entries_to_read = (num_entries as usize).min(remaining);

    let mut out = Vec::with_capacity(entries_to_read);
    for i in 0..entries_to_read {
        let entry_start = header_size as u64 + (i as u64) * entry_length as u64;
        cur.set_position(entry_start);

        let play_count = cur.read_u32::<LittleEndian>()?;
        let last_played = cur.read_u32::<LittleEndian>()?;
        let _audiobook_speed = cur.read_u32::<LittleEndian>()?;
        let rating = cur.read_u32::<LittleEndian>()?;

        let mut skip_count = 0u32;
        let mut last_skipped = 0u32;
        if entry_length >= 20 {
            let _bookmark = cur.read_u32::<LittleEndian>()?;
        }
        if entry_length >= 24 {
            let _play_count_total = cur.read_u32::<LittleEndian>()?;
        }
        if entry_length >= 28 {
            skip_count = cur.read_u32::<LittleEndian>()?;
        }
        if entry_length >= 32 {
            last_skipped = cur.read_u32::<LittleEndian>()?;
        }

        out.push(PlayCountEntry {
            play_count,
            last_played,
            rating,
            skip_count,
            last_skipped,
        });
    }

    Ok(out)
}

/// Fold sidecar entries into a track list, in place. Entries align
/// positionally to `tracks` — the N-th entry updates the N-th track.
///
/// Semantics:
/// - `play_count` and `skip_count` are **deltas** the firmware accumulated
///   since the last sync; they are added to the existing mhit values.
/// - `last_played` and `last_skipped` are **timestamps**; the larger value
///   wins (firmware advanced the timestamp iff it observed a play/skip).
/// - `rating` is a **state**; the sidecar value overrides mhit unless it is
///   the `RATING_UNSET` sentinel (0xFF), in which case the mhit value is
///   preserved.
///
/// If the entry count does not match the track count, the merge is skipped
/// (returns `false`) — a length mismatch means the sidecar was written
/// against a different iTunesDB than we just parsed and applying it would
/// silently corrupt every track's counters.
pub fn apply_to_tracks(tracks: &mut [IpodTrack], entries: &[PlayCountEntry]) -> bool {
    if entries.len() != tracks.len() {
        return false;
    }
    for (track, entry) in tracks.iter_mut().zip(entries.iter()) {
        track.play_count = track.play_count.saturating_add(entry.play_count);
        if entry.last_played > track.last_played {
            track.last_played = entry.last_played;
        }
        track.skip_count = track.skip_count.saturating_add(entry.skip_count);
        if entry.last_skipped > track.last_skipped {
            track.last_skipped = entry.last_skipped;
        }
        if entry.rating != RATING_UNSET {
            // Clamp to u8 since the on-disk mhit byte is one byte wide.
            track.rating = (entry.rating & 0xFF) as u8;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use byteorder::WriteBytesExt;

    /// Build a Play Counts file with the given entry layout.
    fn build_sidecar(entry_length: u32, entries: &[Vec<u32>]) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"mhdp");
        buf.write_u32::<LittleEndian>(96).unwrap();
        buf.write_u32::<LittleEndian>(entry_length).unwrap();
        buf.write_u32::<LittleEndian>(entries.len() as u32).unwrap();
        // Pad header out to 96 bytes.
        for _ in 0..((96 - 16) / 4) {
            buf.write_u32::<LittleEndian>(0).unwrap();
        }
        for fields in entries {
            // Pad field list to entry_length / 4 u32s.
            let want = (entry_length / 4) as usize;
            for j in 0..want {
                let v = fields.get(j).copied().unwrap_or(0);
                buf.write_u32::<LittleEndian>(v).unwrap();
            }
        }
        buf
    }

    #[test]
    fn parse_empty_file_returns_empty_vec() {
        let bytes = build_sidecar(28, &[]);
        let entries = parse(&bytes).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn parse_minimum_16_byte_entries() {
        // 16-byte entry: play_count, last_played, audiobook_speed, rating.
        // No skip_count, no last_skipped.
        let bytes = build_sidecar(
            16,
            &[
                vec![3, 0xAABB_CCDD, 0, 60],
                vec![1, 0x1111_2222, 0, RATING_UNSET],
            ],
        );
        let entries = parse(&bytes).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].play_count, 3);
        assert_eq!(entries[0].last_played, 0xAABB_CCDD);
        assert_eq!(entries[0].rating, 60);
        assert_eq!(entries[0].skip_count, 0);
        assert_eq!(entries[0].last_skipped, 0);
        assert_eq!(entries[1].rating, RATING_UNSET);
    }

    #[test]
    fn parse_28_byte_entries_includes_skip_count() {
        let bytes = build_sidecar(28, &[vec![5, 100, 0, 80, 0, 99, 2]]);
        let entries = parse(&bytes).unwrap();
        assert_eq!(entries[0].play_count, 5);
        assert_eq!(entries[0].skip_count, 2);
        assert_eq!(entries[0].last_skipped, 0);
    }

    #[test]
    fn parse_32_byte_entries_includes_last_skipped() {
        let bytes = build_sidecar(32, &[vec![1, 50, 0, 0, 0, 0, 1, 250]]);
        let entries = parse(&bytes).unwrap();
        assert_eq!(entries[0].skip_count, 1);
        assert_eq!(entries[0].last_skipped, 250);
    }

    #[test]
    fn parse_rejects_bad_magic() {
        let mut bytes = build_sidecar(28, &[]);
        bytes[..4].copy_from_slice(b"XXXX");
        assert!(parse(&bytes).is_err());
    }

    #[test]
    fn parse_rejects_truncated_header() {
        let bytes = vec![0u8; 8];
        assert!(parse(&bytes).is_err());
    }

    #[test]
    fn parse_rejects_tiny_entry_length() {
        let mut bytes = build_sidecar(28, &[]);
        // Set entry_length to 8 (invalid — must be >= 16).
        bytes[8..12].copy_from_slice(&8u32.to_le_bytes());
        assert!(parse(&bytes).is_err());
    }

    #[test]
    fn parse_clamps_oversized_num_entries() {
        // Header claims 0xFFFF entries but the file is short — the parser
        // must not allocate huge buffers or panic.
        let mut bytes = build_sidecar(28, &[vec![1, 0, 0, 0, 0, 0, 0]]);
        // Override num_entries to 0xFFFF in the header.
        bytes[12..16].copy_from_slice(&0xFFFFu32.to_le_bytes());
        let entries = parse(&bytes).unwrap();
        assert_eq!(entries.len(), 1); // clamped to actual buffer capacity
    }

    fn track_with(play_count: u32, skip_count: u32, last_played: u32, rating: u8) -> IpodTrack {
        IpodTrack {
            play_count,
            skip_count,
            last_played,
            rating,
            ..IpodTrack::default()
        }
    }

    #[test]
    fn apply_adds_play_and_skip_counts() {
        let mut tracks = vec![track_with(5, 1, 100, 0), track_with(0, 0, 0, 60)];
        let entries = vec![
            PlayCountEntry {
                play_count: 2,
                last_played: 200,
                rating: RATING_UNSET,
                skip_count: 1,
                last_skipped: 150,
            },
            PlayCountEntry {
                play_count: 0,
                last_played: 0,
                rating: 80,
                skip_count: 0,
                last_skipped: 0,
            },
        ];
        assert!(apply_to_tracks(&mut tracks, &entries));
        assert_eq!(tracks[0].play_count, 7);
        assert_eq!(tracks[0].skip_count, 2);
        assert_eq!(tracks[0].last_played, 200);
        assert_eq!(tracks[0].last_skipped, 150);
        // RATING_UNSET keeps the existing rating.
        assert_eq!(tracks[0].rating, 0);
        // Non-sentinel rating overrides.
        assert_eq!(tracks[1].rating, 80);
    }

    #[test]
    fn apply_keeps_larger_last_played() {
        let mut tracks = vec![track_with(0, 0, 500, 0)];
        let entries = vec![PlayCountEntry {
            play_count: 1,
            last_played: 300, // older than track's last_played
            rating: RATING_UNSET,
            skip_count: 0,
            last_skipped: 0,
        }];
        assert!(apply_to_tracks(&mut tracks, &entries));
        // Existing 500 wins over sidecar's 300.
        assert_eq!(tracks[0].last_played, 500);
    }

    #[test]
    fn apply_skips_on_length_mismatch() {
        let mut tracks = vec![track_with(5, 0, 0, 0)];
        let entries = vec![
            PlayCountEntry {
                play_count: 1,
                last_played: 0,
                rating: RATING_UNSET,
                skip_count: 0,
                last_skipped: 0,
            },
            PlayCountEntry::default(),
        ];
        assert!(!apply_to_tracks(&mut tracks, &entries));
        // Track unchanged because we refused to merge.
        assert_eq!(tracks[0].play_count, 5);
    }

    #[test]
    fn apply_saturates_on_overflow() {
        let mut tracks = vec![track_with(u32::MAX - 1, 0, 0, 0)];
        let entries = vec![PlayCountEntry {
            play_count: 100,
            last_played: 0,
            rating: RATING_UNSET,
            skip_count: 0,
            last_skipped: 0,
        }];
        assert!(apply_to_tracks(&mut tracks, &entries));
        assert_eq!(tracks[0].play_count, u32::MAX);
    }
}
