//! Loading-screen phrase rotator. Cycles a set of jokey `Scoping <album>` /
//! `Vibing with <artist>` prefixes against a rolling window of track samples
//! the dirlib scanner emits, so the user has something to read while a large
//! library is being indexed.

use std::time::{SystemTime, UNIX_EPOCH};

use zytunes::dirlib::TrackSample;

/// Which field of a track sample a loading phrase refers to.
#[derive(Copy, Clone)]
enum ScanField {
    Artist,
    Album,
    Track,
}

struct ScanPhrase {
    prefix: &'static str,
    field: ScanField,
}

/// Fun loading-phrase prefixes, cycled while the library is scanning.
const SCAN_PHRASES: &[ScanPhrase] = &[
    ScanPhrase {
        prefix: "Scoping",
        field: ScanField::Album,
    },
    ScanPhrase {
        prefix: "Scanning",
        field: ScanField::Artist,
    },
    ScanPhrase {
        prefix: "Creepin' on",
        field: ScanField::Artist,
    },
    ScanPhrase {
        prefix: "Puttin' a spell on",
        field: ScanField::Track,
    },
    ScanPhrase {
        prefix: "Vibing with",
        field: ScanField::Artist,
    },
    ScanPhrase {
        prefix: "Peeking at",
        field: ScanField::Album,
    },
    ScanPhrase {
        prefix: "Digging through",
        field: ScanField::Artist,
    },
    ScanPhrase {
        prefix: "Unpacking",
        field: ScanField::Album,
    },
    ScanPhrase {
        prefix: "Snooping on",
        field: ScanField::Track,
    },
    ScanPhrase {
        prefix: "Cataloging",
        field: ScanField::Artist,
    },
    ScanPhrase {
        prefix: "Tipping hat to",
        field: ScanField::Track,
    },
];

/// Minimum time a scan phrase stays on screen before rotating (milliseconds).
pub(super) const SCAN_PHRASE_MS: u128 = 900;

/// Pick a phrase at random and pair it with one of `samples`, returning a
/// formatted `<prefix> <subject>` string. Returns `None` if `samples` is empty.
pub(super) fn pick_phrase(samples: &[TrackSample]) -> Option<String> {
    if samples.is_empty() {
        return None;
    }
    let seed = quick_random();
    let phrase = &SCAN_PHRASES[seed % SCAN_PHRASES.len()];
    let sample = &samples[(seed / 7) % samples.len()];
    let target: &str = match phrase.field {
        ScanField::Artist => &sample.artist,
        ScanField::Album => &sample.album,
        ScanField::Track => &sample.name,
    };
    Some(format!("{} {}", phrase.prefix, target))
}

/// Cheap entropy source for picking phrases and samples; quality doesn't matter.
fn quick_random() -> usize {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as usize)
        .unwrap_or(0)
}
