//! Platform CD-drive enumeration and TOC reading via libdiscid.
//!
//! Backed by the [`discid`](https://crates.io/crates/discid) crate which
//! wraps `libdiscid` (LGPL, dynamically linked, attributed in
//! `THIRD_PARTY.md`). The Phase 0 stubs are replaced here with real
//! implementations on macOS and Linux; other platforms still return empty.
//!
//! ## Drive enumeration
//!
//! libdiscid exposes `default_device()` — the OS's first/default optical
//! drive. We surface that as a single [`CdDrive`] entry. Multi-drive
//! enumeration would require platform-specific APIs (DiskArbitration on
//! macOS, sysfs walking on Linux); Phase 1 punts on that since the
//! overwhelming majority of host machines have one optical drive at most.
//!
//! ## TOC reading and media presence
//!
//! libdiscid's `read()` is the single entry point for both — a successful
//! read implies media is present and yields the full TOC; failure with a
//! "no medium" message implies the drive is empty. We pattern-match on the
//! error string to distinguish empty-drive from real IO errors.

use std::path::PathBuf;

use super::discid::{DiscToc, TocTrack};

/// A CD/DVD drive present on the system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CdDrive {
    /// OS device path (`/dev/disk4`, `/dev/sr0`).
    pub path: PathBuf,
    /// Human-readable name for the TUI status line.
    pub name: String,
    /// Whether the drive currently reports media present. `None` = unknown
    /// (no probe attempted yet); set by [`read_disc_toc`].
    pub media_present: Option<bool>,
}

/// Errors when interacting with a drive.
#[derive(Debug)]
pub enum DriveError {
    /// The OS surface for talking to optical drives is not yet implemented
    /// on this platform.
    Unsupported(&'static str),
    /// Drive was found but has no disc loaded.
    NoMedia,
    /// IO / libdiscid error talking to the drive.
    Io(String),
}

impl std::fmt::Display for DriveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DriveError::Unsupported(p) => write!(f, "CD drive access not implemented on {p}"),
            DriveError::NoMedia => write!(f, "no disc in drive"),
            DriveError::Io(e) => write!(f, "drive IO error: {e}"),
        }
    }
}

impl std::error::Error for DriveError {}

/// Enumerate optical drives present on the system. Empty vec is a valid
/// result — most laptops have no optical drive at all.
pub fn enumerate_drives() -> Vec<CdDrive> {
    backend::enumerate_drives()
}

/// Read the table of contents from a disc in `drive`.
///
/// Returns [`DriveError::NoMedia`] when the drive is empty and
/// [`DriveError::Io`] for any other libdiscid failure (permission denied,
/// bad device path, IO error during the SCSI READ TOC).
pub fn read_disc_toc(drive: &CdDrive) -> Result<DiscToc, DriveError> {
    backend::read_disc_toc(drive)
}

/// Classifier for libdiscid error messages. Pattern-matches on the strings
/// libdiscid surfaces to separate "drive is empty" from real IO failures.
/// Lives outside the `cfg` backends so it can be unit-tested on any
/// platform.
fn classify_libdiscid_error(message: &str) -> DriveError {
    let lower = message.to_ascii_lowercase();
    if lower.contains("no medium")
        || lower.contains("no media")
        || lower.contains("no disc")
        || lower.contains("not ready")
        || lower.contains("empty")
    {
        DriveError::NoMedia
    } else {
        DriveError::Io(message.to_string())
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod backend {
    use super::*;
    use discid::DiscId;

    pub fn enumerate_drives() -> Vec<CdDrive> {
        let default = DiscId::default_device();
        if default.is_empty() {
            return Vec::new();
        }
        vec![CdDrive {
            path: PathBuf::from(&default),
            name: format!("Optical Drive ({default})"),
            media_present: None,
        }]
    }

    pub fn read_disc_toc(drive: &CdDrive) -> Result<DiscToc, DriveError> {
        let device = drive
            .path
            .to_str()
            .ok_or_else(|| DriveError::Io("device path is not UTF-8".into()))?;
        let disc =
            DiscId::read(Some(device)).map_err(|e| classify_libdiscid_error(&e.to_string()))?;
        Ok(disc_to_toc(&disc))
    }

    /// Convert a libdiscid [`DiscId`] into our pure-Rust [`DiscToc`] shape.
    ///
    /// Crate types use `i32`; the values are always non-negative in practice
    /// (track numbers 1..=99 per Red Book, LBA offsets fit in `u32`).
    /// `clamp_u8` and `clamp_u32` saturate rather than truncate so a
    /// nonsensical libdiscid value (out-of-range track number, negative LBA)
    /// produces a recognisably-wrong TOC instead of silently wrapping.
    pub(super) fn disc_to_toc(disc: &DiscId) -> DiscToc {
        DiscToc {
            first_track: clamp_u8(disc.first_track_num()),
            last_track: clamp_u8(disc.last_track_num()),
            lead_out_lba: clamp_u32(disc.sectors()),
            tracks: disc
                .tracks()
                .map(|t| TocTrack {
                    number: clamp_u8(t.number),
                    offset_lba: clamp_u32(t.offset),
                })
                .collect(),
        }
    }

    fn clamp_u8(v: i32) -> u8 {
        v.clamp(0, u8::MAX as i32) as u8
    }

    fn clamp_u32(v: i32) -> u32 {
        v.max(0) as u32
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod backend {
    use super::*;

    pub fn enumerate_drives() -> Vec<CdDrive> {
        Vec::new()
    }

    pub fn read_disc_toc(_drive: &CdDrive) -> Result<DiscToc, DriveError> {
        Err(DriveError::Unsupported("this platform"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enumerate_returns_a_vec() {
        // Doesn't assert non-empty — CI runners may have no optical drives — but
        // exercises the platform dispatch and guarantees the API does not panic.
        let _ = enumerate_drives();
    }

    #[test]
    fn unsupported_error_display_is_helpful() {
        let err = DriveError::Unsupported("FooOS");
        assert!(format!("{err}").contains("FooOS"));
    }

    #[test]
    fn classify_no_medium_variants() {
        for msg in [
            "No medium found",
            "no medium found",
            "drive reports: NO MEDIA",
            "Disc not ready",
            "drive is empty",
            "No disc inserted",
        ] {
            assert!(
                matches!(classify_libdiscid_error(msg), DriveError::NoMedia),
                "should be NoMedia: {msg}"
            );
        }
    }

    #[test]
    fn classify_other_errors_pass_through() {
        for msg in [
            "Permission denied",
            "Device or resource busy",
            "Invalid SCSI response",
        ] {
            match classify_libdiscid_error(msg) {
                DriveError::Io(s) => assert_eq!(s, msg),
                other => panic!("expected Io variant for {msg:?}, got {other:?}"),
            }
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn disc_to_toc_round_trips_known_offsets() {
        // libdiscid's `put` constructs a DiscId from raw values without
        // touching hardware — perfect for testing our conversion path.
        // Convention: offsets[0] = lead-out LBA, offsets[1..] = track 1..N.
        use discid::DiscId;
        let offsets = [258725_i32, 150, 17510, 33275, 45910, 57805];
        let disc = DiscId::put(1, &offsets).expect("put with valid offsets");
        let toc = backend::disc_to_toc(&disc);
        assert_eq!(toc.first_track, 1);
        assert_eq!(toc.last_track, 5);
        assert_eq!(toc.lead_out_lba, 258725);
        assert_eq!(toc.tracks.len(), 5);
        assert_eq!(toc.tracks[0].number, 1);
        assert_eq!(toc.tracks[0].offset_lba, 150);
        assert_eq!(toc.tracks[4].offset_lba, 57805);
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn disc_to_toc_then_compute_disc_id_matches_libdiscid() {
        // Cross-check: our pure-Rust `compute_disc_id` (Phase 0) and
        // libdiscid's `id()` should produce identical MB disc IDs for the
        // same TOC. Validates the conversion preserves all bits the
        // algorithm depends on.
        use crate::cd::discid::compute_disc_id;
        use discid::DiscId;
        let offsets = [
            258725_i32, 150, 17510, 33275, 45910, 57805, 78310, 94650, 109580, 132010, 149160,
            165115, 177710, 203325, 215555, 235590,
        ];
        let disc = DiscId::put(1, &offsets).expect("put with valid offsets");
        let toc = backend::disc_to_toc(&disc);
        let our_id = compute_disc_id(&toc);
        let lib_id = disc.id();
        assert_eq!(
            our_id, lib_id,
            "pure-Rust compute_disc_id should agree with libdiscid for the same TOC"
        );
    }
}
