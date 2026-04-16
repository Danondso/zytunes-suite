//! zune-mtp: Native MTP/MTPZ library for USB device communication.
//!
//! On macOS, uses Apple's IOKit framework for USB bulk I/O (bypassing libusb
//! which fails on data-out operations for MTPZ-authenticated devices).
//! On Linux, uses libusb (via rusb) which handles bulk I/O natively.

use std::fmt;

/// Errors from MTP/MTPZ operations.
#[derive(Debug)]
pub enum MtpError {
    /// USB transport errors (open, read, write, timeout).
    Usb(String),
    /// MTP protocol-level errors (bad response codes, malformed data).
    Protocol(String),
    /// Device responded to an operation with an MTP response code other than OK.
    /// Typically `0x2005 OperationNotSupported` for vendor ops the firmware
    /// doesn't implement — callers can treat these as "feature unavailable"
    /// rather than fatal protocol errors.
    DeviceRejected(u16),
    /// MTPZ cryptographic handshake errors.
    Crypto(String),
    /// Key file loading errors.
    KeyLoad(String),
    /// I/O errors.
    Io(std::io::Error),
}

impl MtpError {
    /// True if the device replied with `OperationNotSupported (0x2005)`.
    pub fn is_operation_not_supported(&self) -> bool {
        matches!(self, MtpError::DeviceRejected(0x2005))
    }
}

impl fmt::Display for MtpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MtpError::Usb(msg) => write!(f, "USB error: {msg}"),
            MtpError::Protocol(msg) => write!(f, "MTP protocol error: {msg}"),
            MtpError::DeviceRejected(code) => write!(f, "device rejected operation (0x{code:04x})"),
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

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
compile_error!("zune-mtp only supports macOS and Linux");

pub mod container;
pub mod mtpz;
pub mod proplist;
pub mod session;
pub mod transport;

pub use mtpz::MtpzKeys;
pub use session::{MtpSession, ObjectInfo};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_rejected_classifies_operation_not_supported() {
        assert!(MtpError::DeviceRejected(0x2005).is_operation_not_supported());
        // Any other rejection code is not "unsupported" — callers should
        // surface these instead of swallowing them.
        assert!(!MtpError::DeviceRejected(0x2002).is_operation_not_supported()); // GeneralError
        assert!(!MtpError::DeviceRejected(0x200f).is_operation_not_supported()); // SessionNotOpen
        assert!(!MtpError::Protocol("bad header".into()).is_operation_not_supported());
    }

    #[test]
    fn device_rejected_display_includes_code() {
        let msg = format!("{}", MtpError::DeviceRejected(0x2005));
        assert!(msg.contains("0x2005"), "expected hex code, got {msg:?}");
        assert!(msg.contains("device rejected"));
    }
}
