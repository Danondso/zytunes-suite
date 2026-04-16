pub mod ipod;
pub mod zune;

pub use ipod::IpodBackend;
pub use zune::{zune_model_from_storage, ZuneBackend, ZuneDetectError, ZuneDevice, ZuneDeviceData};

use crate::mtp::DeviceSession;

/// Device family identifier for branching on device-specific behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceFamily {
    Zune,
    Ipod,
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
