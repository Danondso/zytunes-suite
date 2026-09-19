//! Pure projection of `GetObjectPropList` results onto `DeviceEntry` rows.
//!
//! Each function pulls a single MTP object property (UseCount, Rating,
//! SkipCount, Duration) out of the parsed prop-list response and stamps it onto the
//! matching track by `object_id`. They share the same shape on purpose:
//! `enrich_one_prop` in `native.rs` is generic over them and counts how
//! many tracks each pass populated for the log line.
//!
//! Tracks with `object_id == 0` (e.g. ZMDB rows on a fresh connect, before
//! any cache merge) are skipped — a device that erroneously reported a
//! handle of 0 must not silently overwrite their fields.

use std::collections::HashMap;

use zune_mtp::proplist::{PROP_DURATION, PROP_RATING, PROP_SKIP_COUNT, PROP_USE_COUNT};
use zune_mtp::session::PropListElement;

use crate::mtp::parse::DeviceEntry;

/// Project parsed `GetObjectPropList` elements onto tracks, populating
/// `play_count` for entries whose `object_id` matches a returned handle.
/// Tracks without an `object_id` (ZMDB entries on a fresh connect) keep
/// `play_count = None`. Pure helper so the join logic is unit-testable
/// without a live MTP session.
pub(super) fn apply_playcounts(tracks: &mut [DeviceEntry], elements: &[PropListElement]) {
    let mut counts: HashMap<u32, u32> = HashMap::new();
    for e in elements {
        if e.prop_code == PROP_USE_COUNT {
            if let Some(v) = e.as_u32() {
                counts.insert(e.object_handle, v);
            }
        }
    }
    if counts.is_empty() {
        return;
    }
    for t in tracks.iter_mut() {
        if t.object_id == 0 {
            continue;
        }
        if let Some(&n) = counts.get(&(t.object_id as u32)) {
            t.play_count = Some(n);
        }
    }
}

/// Project parsed `GetObjectPropList` elements onto tracks, populating
/// `rating` for entries whose `object_id` matches a returned handle. Same
/// shape as `apply_playcounts` but for `0xDC8A Rating` (UINT16). Tracks
/// without `object_id` are skipped silently.
pub(super) fn apply_ratings(tracks: &mut [DeviceEntry], elements: &[PropListElement]) {
    let mut ratings: HashMap<u32, u16> = HashMap::new();
    for e in elements {
        if e.prop_code == PROP_RATING {
            if let Some(v) = e.as_u16() {
                ratings.insert(e.object_handle, v);
            }
        }
    }
    if ratings.is_empty() {
        return;
    }
    for t in tracks.iter_mut() {
        if t.object_id == 0 {
            continue;
        }
        if let Some(&r) = ratings.get(&(t.object_id as u32)) {
            t.rating = Some(r);
        }
    }
}

/// Project parsed `GetObjectPropList` elements onto tracks, populating
/// `skip_count` for entries whose `object_id` matches a returned handle.
/// Same shape as `apply_playcounts` but for `0xDC92 SkipCount` (UINT32).
/// Tracks without `object_id` are skipped silently.
pub(super) fn apply_skip_counts(tracks: &mut [DeviceEntry], elements: &[PropListElement]) {
    let mut counts: HashMap<u32, u32> = HashMap::new();
    for e in elements {
        if e.prop_code == PROP_SKIP_COUNT {
            if let Some(v) = e.as_u32() {
                counts.insert(e.object_handle, v);
            }
        }
    }
    if counts.is_empty() {
        return;
    }
    for t in tracks.iter_mut() {
        if t.object_id == 0 {
            continue;
        }
        if let Some(&n) = counts.get(&(t.object_id as u32)) {
            t.skip_count = Some(n);
        }
    }
}

/// Project parsed `GetObjectPropList` elements onto tracks, populating
/// `duration_ms` for entries whose `object_id` matches a returned handle.
/// MTP `0xDC89 Duration` is UINT32 milliseconds. A reported `0` is skipped
/// — duration 0 is meaningless and would hide the library-tag fallback.
pub(super) fn apply_durations(tracks: &mut [DeviceEntry], elements: &[PropListElement]) {
    let mut durations: HashMap<u32, u32> = HashMap::new();
    for e in elements {
        if e.prop_code == PROP_DURATION {
            if let Some(v) = e.as_u32() {
                if v > 0 {
                    durations.insert(e.object_handle, v);
                }
            }
        }
    }
    if durations.is_empty() {
        return;
    }
    for t in tracks.iter_mut() {
        if t.object_id == 0 {
            continue;
        }
        if let Some(&ms) = durations.get(&(t.object_id as u32)) {
            t.duration_ms = Some(ms);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_entry(name: &str, id: u64) -> DeviceEntry {
        DeviceEntry {
            object_id: id,
            storage_id: 65537,
            format: "MP3".to_string(),
            size: 1024,
            name: name.to_string(),
            ..Default::default()
        }
    }

    fn use_count_element(handle: u32, count: u32) -> PropListElement {
        PropListElement {
            object_handle: handle,
            prop_code: PROP_USE_COUNT,
            datatype: 0x0006,
            value: count.to_le_bytes().to_vec(),
        }
    }

    #[test]
    fn apply_playcounts_populates_matched_handles() {
        let mut tracks = vec![sample_entry("a.mp3", 100), sample_entry("b.mp3", 101)];
        let elements = vec![use_count_element(100, 7), use_count_element(101, 0)];
        apply_playcounts(&mut tracks, &elements);
        assert_eq!(tracks[0].play_count, Some(7));
        assert_eq!(tracks[1].play_count, Some(0));
    }

    #[test]
    fn apply_playcounts_leaves_unmatched_handles_alone() {
        // Track 102 has no element in the prop-list response; it should
        // stay None, not be cleared, even if play_count was already set.
        let mut tracks = vec![
            sample_entry("a.mp3", 100),
            DeviceEntry {
                play_count: Some(99),
                ..sample_entry("b.mp3", 102)
            },
        ];
        let elements = vec![use_count_element(100, 5)];
        apply_playcounts(&mut tracks, &elements);
        assert_eq!(tracks[0].play_count, Some(5));
        assert_eq!(tracks[1].play_count, Some(99));
    }

    #[test]
    fn apply_playcounts_skips_zero_object_ids() {
        // ZMDB tracks before any cache merge have object_id == 0; they must
        // not match an element whose handle was reported as 0 by the device.
        let mut tracks = vec![sample_entry("zmdb-only.mp3", 0)];
        let elements = vec![use_count_element(0, 12)];
        apply_playcounts(&mut tracks, &elements);
        assert_eq!(tracks[0].play_count, None);
    }

    #[test]
    fn apply_playcounts_ignores_non_use_count_props() {
        let mut tracks = vec![sample_entry("a.mp3", 200)];
        let rating = PropListElement {
            object_handle: 200,
            prop_code: PROP_RATING,
            datatype: 0x0004,
            value: 4u16.to_le_bytes().to_vec(),
        };
        apply_playcounts(&mut tracks, &[rating]);
        assert_eq!(tracks[0].play_count, None);
    }

    #[test]
    fn apply_ratings_populates_matched_handles() {
        let mut tracks = vec![sample_entry("a.mp3", 300), sample_entry("b.mp3", 301)];
        let elements = vec![
            PropListElement {
                object_handle: 300,
                prop_code: PROP_RATING,
                datatype: 0x0004,
                value: 80u16.to_le_bytes().to_vec(),
            },
            PropListElement {
                object_handle: 301,
                prop_code: PROP_RATING,
                datatype: 0x0004,
                value: 0u16.to_le_bytes().to_vec(),
            },
        ];
        apply_ratings(&mut tracks, &elements);
        assert_eq!(tracks[0].rating, Some(80));
        assert_eq!(tracks[1].rating, Some(0));
    }

    #[test]
    fn apply_ratings_ignores_use_count_props() {
        // A UseCount element must not be misinterpreted as a rating.
        let mut tracks = vec![sample_entry("a.mp3", 400)];
        let use_count = PropListElement {
            object_handle: 400,
            prop_code: PROP_USE_COUNT,
            datatype: 0x0006,
            value: 5u32.to_le_bytes().to_vec(),
        };
        apply_ratings(&mut tracks, &[use_count]);
        assert_eq!(tracks[0].rating, None);
    }

    #[test]
    fn apply_skip_counts_populates_matched_handles() {
        let mut tracks = vec![
            sample_entry("a.mp3", 500),
            sample_entry("b.mp3", 501),
            sample_entry("c.mp3", 502),
        ];
        let elements = vec![
            PropListElement {
                object_handle: 500,
                prop_code: PROP_SKIP_COUNT,
                datatype: 0x0006,
                value: 3u32.to_le_bytes().to_vec(),
            },
            PropListElement {
                object_handle: 502,
                prop_code: PROP_SKIP_COUNT,
                datatype: 0x0006,
                value: 0u32.to_le_bytes().to_vec(),
            },
        ];
        apply_skip_counts(&mut tracks, &elements);
        assert_eq!(tracks[0].skip_count, Some(3));
        assert_eq!(tracks[1].skip_count, None, "unmatched handle stays None");
        assert_eq!(tracks[2].skip_count, Some(0));
    }

    #[test]
    fn apply_skip_counts_skips_zero_object_ids() {
        // ZMDB tracks land with object_id=0 before any cache merge — they
        // must stay None even when an element has handle=0.
        let mut tracks = vec![DeviceEntry {
            object_id: 0,
            ..sample_entry("a.mp3", 0)
        }];
        let elements = vec![PropListElement {
            object_handle: 0,
            prop_code: PROP_SKIP_COUNT,
            datatype: 0x0006,
            value: 5u32.to_le_bytes().to_vec(),
        }];
        apply_skip_counts(&mut tracks, &elements);
        assert_eq!(tracks[0].skip_count, None);
    }

    #[test]
    fn apply_skip_counts_ignores_non_skip_count_props() {
        // A UseCount element must not be misinterpreted as a skip count.
        let mut tracks = vec![sample_entry("a.mp3", 600)];
        let use_count = PropListElement {
            object_handle: 600,
            prop_code: PROP_USE_COUNT,
            datatype: 0x0006,
            value: 5u32.to_le_bytes().to_vec(),
        };
        apply_skip_counts(&mut tracks, &[use_count]);
        assert_eq!(tracks[0].skip_count, None);
    }

    fn duration_element(handle: u32, ms: u32) -> PropListElement {
        PropListElement {
            object_handle: handle,
            prop_code: PROP_DURATION,
            datatype: 0x0006,
            value: ms.to_le_bytes().to_vec(),
        }
    }

    #[test]
    fn apply_durations_populates_matched_handles() {
        let mut tracks = vec![sample_entry("a.mp3", 700), sample_entry("b.mp3", 701)];
        let elements = vec![duration_element(700, 240_000), duration_element(701, 1_000)];
        apply_durations(&mut tracks, &elements);
        assert_eq!(tracks[0].duration_ms, Some(240_000));
        assert_eq!(tracks[1].duration_ms, Some(1_000));
    }

    #[test]
    fn apply_durations_skips_zero() {
        // Duration 0 is meaningless; leave the field alone so a library
        // fallback can still fill the TUI column.
        let mut tracks = vec![
            DeviceEntry {
                duration_ms: Some(180_000),
                ..sample_entry("a.mp3", 800)
            },
            sample_entry("b.mp3", 801),
        ];
        apply_durations(
            &mut tracks,
            &[duration_element(800, 0), duration_element(801, 0)],
        );
        assert_eq!(tracks[0].duration_ms, Some(180_000));
        assert_eq!(tracks[1].duration_ms, None);
    }

    #[test]
    fn apply_durations_skips_zero_object_ids() {
        let mut tracks = vec![sample_entry("zmdb-only.mp3", 0)];
        apply_durations(&mut tracks, &[duration_element(0, 240_000)]);
        assert_eq!(tracks[0].duration_ms, None);
    }

    #[test]
    fn apply_durations_ignores_non_duration_props() {
        let mut tracks = vec![sample_entry("a.mp3", 900)];
        let use_count = PropListElement {
            object_handle: 900,
            prop_code: PROP_USE_COUNT,
            datatype: 0x0006,
            value: 5u32.to_le_bytes().to_vec(),
        };
        apply_durations(&mut tracks, &[use_count]);
        assert_eq!(tracks[0].duration_ms, None);
    }
}
