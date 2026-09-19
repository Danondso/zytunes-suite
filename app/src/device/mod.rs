pub mod ipod;
mod ipod_models;
pub mod zune;

pub use ipod::{IpodBackend, IpodDeviceData};
pub use ipod_models::{ipod_model_from_storage, ipod_model_label};
pub use zune::{zune_model_from_storage, ZuneBackend, ZuneDetectError, ZuneDevice, ZuneDeviceData};

use crate::mtp::DeviceSession;

/// Device family identifier for branching on device-specific behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceFamily {
    Zune,
    Ipod,
}

impl DeviceFamily {
    /// Short label for UI copy ("Zune" / "iPod").
    pub fn label(self) -> &'static str {
        match self {
            DeviceFamily::Zune => "Zune",
            DeviceFamily::Ipod => "iPod",
        }
    }
}

/// Capabilities of a specific device family, used to drive format decisions,
/// transcoding, and path layout without hardcoding Zune assumptions.
#[derive(Clone)]
pub struct DeviceCapabilities {
    pub family: DeviceFamily,
    pub supported_formats: &'static [&'static str],
    pub transcode_target: &'static str,
    pub music_root: &'static str,
    pub max_art_dimensions: Option<(u32, u32)>,
    /// Highest-fidelity *lossless* container the device's firmware accepts
    /// natively. `Some("alac")` for iPod Classic (FLAC sources transcode
    /// up to lossless ALAC on push instead of falling through to lossy
    /// MP3); `None` for the Zune (no native lossless support — sources
    /// always go through the lossy `transcode_target` path).
    pub lossless_target: Option<&'static str>,
}

/// A detected (but not yet connected) device, with backend-specific data
/// stored as a type-erased `Any` for downcasting.
pub struct DetectedDevice {
    pub family: DeviceFamily,
    pub name: String,
    pub model: Option<String>,
    pub serial: Option<String>,
    pub firmware: Option<String>,
    pub backend_data: Box<dyn std::any::Any + Send>,
}

/// Trait for device backends that can detect hardware and open sessions.
pub trait DeviceBackend: Send {
    /// Scan for a connected device of this backend's family.
    fn detect(&self) -> Result<DetectedDevice, String>;
    /// Return the static capabilities for this device family.
    fn capabilities(&self) -> DeviceCapabilities;
    /// Open an MTP/transport session to the detected device.
    fn open_session(
        &self,
        detected: &DetectedDevice,
        log: Option<std::sync::mpsc::Sender<String>>,
    ) -> Result<Box<dyn DeviceSession + Send>, String>;
}

#[cfg(test)]
mod tests {
    use super::DeviceFamily;

    #[test]
    fn family_label() {
        assert_eq!(DeviceFamily::Zune.label(), "Zune");
        assert_eq!(DeviceFamily::Ipod.label(), "iPod");
    }
}
