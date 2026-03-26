//! zune-mtp: Native IOKit MTP/MTPZ library for macOS.
//!
//! Provides direct USB communication with MTP devices using Apple's IOKit
//! framework, bypassing libusb which fails on data-out operations for
//! MTPZ-authenticated devices like the Microsoft Zune.

// TODO: Replace String errors with a typed MtpError enum (thiserror).
// This would cover UsbOpen, UsbWrite, UsbRead, MtpProtocol, Timeout,
// Crypto, KeyLoad, Io, and Other variants. Deferred because it touches
// every function signature in the crate.

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
