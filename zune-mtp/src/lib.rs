//! zune-mtp: Native IOKit MTP/MTPZ library for macOS.
//!
//! Provides direct USB communication with MTP devices using Apple's IOKit
//! framework, bypassing libusb which fails on data-out operations for
//! MTPZ-authenticated devices like the Microsoft Zune.

use std::fmt;

/// Errors from MTP/MTPZ operations.
#[derive(Debug)]
pub enum MtpError {
    /// USB transport errors (open, read, write, timeout).
    Usb(String),
    /// MTP protocol-level errors (bad response codes, malformed data).
    Protocol(String),
    /// MTPZ cryptographic handshake errors.
    Crypto(String),
    /// Key file loading errors.
    KeyLoad(String),
    /// I/O errors.
    Io(std::io::Error),
}

impl fmt::Display for MtpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MtpError::Usb(msg) => write!(f, "USB error: {msg}"),
            MtpError::Protocol(msg) => write!(f, "MTP protocol error: {msg}"),
            MtpError::Crypto(msg) => write!(f, "MTPZ crypto error: {msg}"),
            MtpError::KeyLoad(msg) => write!(f, "Key load error: {msg}"),
            MtpError::Io(e) => write!(f, "I/O error: {e}"),
        }
    }
}

impl std::error::Error for MtpError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            MtpError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for MtpError {
    fn from(e: std::io::Error) -> Self {
        MtpError::Io(e)
    }
}

#[cfg(not(target_os = "macos"))]
compile_error!("zune-mtp only supports macOS");

#[cfg(target_os = "macos")]
pub mod container;
#[cfg(target_os = "macos")]
pub(crate) mod iokit_ffi;
#[cfg(target_os = "macos")]
pub mod mtpz;
#[cfg(target_os = "macos")]
pub mod proplist;
#[cfg(target_os = "macos")]
pub mod session;
#[cfg(target_os = "macos")]
pub mod transport;

#[cfg(target_os = "macos")]
pub use mtpz::MtpzKeys;
#[cfg(target_os = "macos")]
pub use session::{MtpSession, ObjectInfo};
