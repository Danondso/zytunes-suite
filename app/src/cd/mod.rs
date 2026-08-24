//! Audio CD detection, identification, and ripping.
//!
//! Split into three concerns:
//! - [`discid`] — pure-Rust MusicBrainz disc ID computation from a table of
//!   contents, plus the `DiscToc` data shape.
//! - [`drive`] — platform-specific CD-drive enumeration and TOC reading.
//!   macOS uses DiskArbitration + IOKit; Linux uses `/dev/sr*` ioctls;
//!   other platforms return empty.
//! - [`rip`] — ffmpeg shell-out for actually pulling audio off the disc.
//!
//! Phase 0 establishes the data plumbing and tests. Phases 1–3 wire it into
//! the TUI, the background worker, and the import overlay.

pub mod discid;
pub mod drive;
pub mod metadata;
pub mod rip;

pub use discid::{compute_disc_id, DiscToc, TocTrack};
pub use drive::{enumerate_drives, read_disc_toc, CdDrive};
pub use metadata::{ripped_track_destination, tag_ripped_file};
pub use rip::{rip_track, RipFidelity, RipProgress};
