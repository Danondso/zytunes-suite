//! Platform-specific CD-drive enumeration and TOC reading.
//!
//! Backed by:
//! - **macOS** — DiskArbitration framework for drive discovery,
//!   `MMCDeviceInterface` (IOKit) for the SCSI READ TOC command. Phase 0
//!   ships the discovery surface; the IOKit TOC read lands with Phase 1
//!   when there's a UI to drive it.
//! - **Linux** — `/sys/block/sr*` walking + `CDROMREADTOCHDR`/`CDROMREADTOCENTRY`
//!   ioctls on the corresponding `/dev/sr*` node. Same staging strategy.
//! - **Other** — returns empty / `Unsupported`.
//!
//! The data shapes (`CdDrive`, [`crate::cd::discid::DiscToc`]) are stable so
//! the TUI and background worker can compile and exercise the API against a
//! mocked drive list in tests today, and pick up real hardware-backed
//! implementations as they land.

use std::path::PathBuf;

use super::discid::DiscToc;

/// A CD/DVD drive present on the system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CdDrive {
    /// OS device path (`/dev/disk4`, `/dev/sr0`, `\\.\D:`).
    pub path: PathBuf,
    /// Human-readable name from the OS (vendor + model where available).
    pub name: String,
    /// Whether the drive currently reports media present. `None` = unknown.
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
    /// IO / ioctl / IOKit error talking to the drive.
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
/// result on platforms where the implementation isn't wired yet.
pub fn enumerate_drives() -> Vec<CdDrive> {
    backend::enumerate_drives()
}

/// Read the table of contents from a disc in `drive`.
pub fn read_disc_toc(drive: &CdDrive) -> Result<DiscToc, DriveError> {
    backend::read_disc_toc(drive)
}

#[cfg(target_os = "macos")]
mod backend {
    use super::*;

    pub fn enumerate_drives() -> Vec<CdDrive> {
        // Phase 1 wires DiskArbitration here. Returning empty preserves the
        // overall shape so callers (BgCommand::DetectCdDrives, the TUI status
        // line) compile and exercise their idle path today.
        Vec::new()
    }

    pub fn read_disc_toc(_drive: &CdDrive) -> Result<DiscToc, DriveError> {
        Err(DriveError::Unsupported("macOS (Phase 1)"))
    }
}

#[cfg(target_os = "linux")]
mod backend {
    use super::*;

    pub fn enumerate_drives() -> Vec<CdDrive> {
        // Phase 1 wires /sys/block/sr* + CDROMREADTOC ioctls here.
        Vec::new()
    }

    pub fn read_disc_toc(_drive: &CdDrive) -> Result<DiscToc, DriveError> {
        Err(DriveError::Unsupported("Linux (Phase 1)"))
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
        // Doesn't assert non-empty — CI runners have no optical drives — but
        // exercises the platform dispatch and guarantees the API does not panic.
        let _ = enumerate_drives();
    }

    #[test]
    fn unsupported_error_display_is_helpful() {
        let err = DriveError::Unsupported("FooOS");
        assert!(format!("{err}").contains("FooOS"));
    }
}
