use rusb::{Context, UsbContext};
use std::fmt;

use crate::mtp::DeviceSession;

use super::{DetectedDevice, DeviceBackend, DeviceCapabilities, DeviceFamily};

/// Microsoft's USB vendor ID.
const MICROSOFT_VENDOR_ID: u16 = 0x045e;

/// Known Zune product IDs.
///
/// The Zune classic family has appeared under multiple product IDs depending
/// on firmware version and USB mode. This list covers known values; more may
/// exist. Zune HD (0x063e) is a separate platform.
const ZUNE_PRODUCT_IDS: &[(u16, &str)] = &[
    (0x0710, "Zune (media mode)"),
    (0x0711, "Zune (firmware update mode)"),
    (0x0712, "Zune (MTP mode, alternate)"),
    (0x063e, "Zune HD (media mode)"),
];

/// Represents a detected Zune device on the USB bus.
pub struct ZuneDevice {
    pub vendor_id: u16,
    pub product_id: u16,
    pub product_name: Option<String>,
    pub firmware_version: Option<String>,
    pub serial_number: Option<String>,
    pub usb_mode: Option<String>,
}

/// Backend-specific data stored in `DetectedDevice::backend_data`.
pub struct ZuneDeviceData {
    pub product_id: u16,
    pub serial_number: Option<String>,
    pub firmware_version: Option<String>,
    pub usb_mode: Option<String>,
}

/// Identify Zune model from USB product ID and total storage capacity.
///
/// The Zune HD (pid=0x063e) is identified by PID first since it overlaps
/// storage sizes with the flash Zunes (16/32 GB). Other generations fall
/// through to capacity-based identification.
pub fn zune_model_from_storage(total_bytes: u64, product_id: u16) -> &'static str {
    let gb = total_bytes / 1_000_000_000;
    if product_id == 0x063e {
        return match gb {
            0..=20 => "Zune HD 16",
            21..=40 => "Zune HD 32",
            _ => "Zune HD 64",
        };
    }
    match gb {
        0..=5 => "Zune 4",
        6..=12 => "Zune 8",
        13..=20 => "Zune 16",
        21..=40 => "Zune 30",
        41..=100 => "Zune 80",
        101..=140 => "Zune 120",
        _ => "Zune",
    }
}

impl fmt::Display for ZuneDevice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Zune vid=0x{:04x} pid=0x{:04x}",
            self.vendor_id, self.product_id,
        )
    }
}

impl ZuneDevice {
    /// Scan all connected USB devices and return the first detected Zune.
    pub fn find() -> Result<ZuneDevice, ZuneDetectError> {
        let context = Context::new().map_err(|e| ZuneDetectError::UsbError(e.to_string()))?;
        let devices = context
            .devices()
            .map_err(|e| ZuneDetectError::UsbError(e.to_string()))?;

        let mut microsoft_devices: Vec<(u16, u8, u8)> = Vec::new();

        for device in devices.iter() {
            let desc = match device.device_descriptor() {
                Ok(d) => d,
                Err(_) => continue,
            };

            if desc.vendor_id() != MICROSOFT_VENDOR_ID {
                continue;
            }

            let product_id = desc.product_id();

            // Check if this is a known Zune product ID.
            if let Some((_, mode_label)) =
                ZUNE_PRODUCT_IDS.iter().find(|(pid, _)| *pid == product_id)
            {
                let handle = device.open();
                let product_name = handle
                    .as_ref()
                    .ok()
                    .and_then(|h| h.read_product_string_ascii(&desc).ok());
                let serial_number = handle
                    .as_ref()
                    .ok()
                    .and_then(|h| h.read_serial_number_string_ascii(&desc).ok());

                // bcdDevice reports USB device revision, not Zune firmware.
                // Real firmware version is read via MTP property 0xD404 after
                // the MTPZ handshake (see NativeSession::open).

                return Ok(ZuneDevice {
                    vendor_id: MICROSOFT_VENDOR_ID,
                    product_id,
                    product_name,
                    firmware_version: None,
                    serial_number,
                    usb_mode: Some(mode_label.to_string()),
                });
            }

            // Track non-Zune Microsoft devices for diagnostics.
            microsoft_devices.push((product_id, device.bus_number(), device.address()));
        }

        if microsoft_devices.is_empty() {
            Err(ZuneDetectError::NotFound {
                hint: "No Microsoft USB devices found. Is the Zune connected and powered on?"
                    .to_string(),
            })
        } else {
            let details: Vec<String> = microsoft_devices
                .iter()
                .map(|(pid, bus, addr)| format!("  - bus {} addr {} pid=0x{:04x}", bus, addr, pid))
                .collect();
            Err(ZuneDetectError::NotFound {
                hint: format!(
                    "Found {} Microsoft device(s) but none matched known Zune product IDs \
                     (expected one of: {}).\nDevices found:\n{}\n\n\
                     If your Zune is connected, its product ID may differ from what we know. \
                     Please file an issue with the product ID above.",
                    microsoft_devices.len(),
                    ZUNE_PRODUCT_IDS
                        .iter()
                        .map(|(pid, label)| format!("0x{:04x} ({})", pid, label))
                        .collect::<Vec<_>>()
                        .join(", "),
                    details.join("\n"),
                ),
            })
        }
    }
}

#[derive(Debug)]
pub enum ZuneDetectError {
    UsbError(String),
    NotFound { hint: String },
}

impl fmt::Display for ZuneDetectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ZuneDetectError::UsbError(msg) => write!(f, "USB error: {}", msg),
            ZuneDetectError::NotFound { hint } => write!(f, "{}", hint),
        }
    }
}

impl std::error::Error for ZuneDetectError {}

/// Zune device backend implementing the `DeviceBackend` trait.
pub struct ZuneBackend;

impl DeviceBackend for ZuneBackend {
    fn detect(&self) -> Result<DetectedDevice, String> {
        let zune = ZuneDevice::find().map_err(|e| format!("{}", e))?;
        let name = zune
            .product_name
            .clone()
            .unwrap_or_else(|| "Zune".to_string());
        Ok(DetectedDevice {
            family: DeviceFamily::Zune,
            name,
            model: None,
            serial: zune.serial_number.clone(),
            firmware: zune.firmware_version.clone(),
            backend_data: Box::new(ZuneDeviceData {
                product_id: zune.product_id,
                serial_number: zune.serial_number,
                firmware_version: zune.firmware_version,
                usb_mode: zune.usb_mode,
            }),
        })
    }

    fn capabilities(&self) -> DeviceCapabilities {
        DeviceCapabilities {
            family: DeviceFamily::Zune,
            supported_formats: &["mp3", "wma", "aac"],
            transcode_target: "mp3",
            music_root: "/Music",
            max_art_dimensions: Some((200, 200)),
        }
    }

    fn open_session(
        &self,
        detected: &DetectedDevice,
        log: Option<std::sync::mpsc::Sender<String>>,
    ) -> Result<Box<dyn DeviceSession + Send>, String> {
        let data = detected
            .backend_data
            .downcast_ref::<ZuneDeviceData>()
            .ok_or_else(|| "Invalid backend data for Zune".to_string())?;

        let log_fn = move |msg: &str| {
            if let Some(ref tx) = log {
                let _ = tx.send(msg.to_string());
            } else {
                eprintln!("{}", msg);
            }
        };

        let mut session = crate::mtp::NativeSession::open(data.product_id, &log_fn)?;
        session.set_serial(data.serial_number.clone());
        Ok(Box::new(session))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zune_model_from_storage_known_sizes() {
        assert_eq!(zune_model_from_storage(4_000_000_000, 0x0710), "Zune 4");
        assert_eq!(zune_model_from_storage(8_000_000_000, 0x0710), "Zune 8");
        assert_eq!(zune_model_from_storage(16_000_000_000, 0x0710), "Zune 16");
        assert_eq!(zune_model_from_storage(30_000_000_000, 0x0710), "Zune 30");
        assert_eq!(zune_model_from_storage(80_000_000_000, 0x0710), "Zune 80");
        assert_eq!(zune_model_from_storage(120_000_000_000, 0x0710), "Zune 120");
        assert_eq!(zune_model_from_storage(200_000_000_000, 0x0710), "Zune");
    }

    #[test]
    fn zune_hd_identified_by_pid() {
        assert_eq!(
            zune_model_from_storage(16_000_000_000, 0x063e),
            "Zune HD 16"
        );
        assert_eq!(
            zune_model_from_storage(32_000_000_000, 0x063e),
            "Zune HD 32"
        );
        assert_eq!(
            zune_model_from_storage(64_000_000_000, 0x063e),
            "Zune HD 64"
        );
    }

    #[test]
    fn display_formatting() {
        let device = ZuneDevice {
            vendor_id: 0x045e,
            product_id: 0x0710,
            product_name: None,
            firmware_version: None,
            serial_number: None,
            usb_mode: None,
        };
        assert_eq!(format!("{}", device), "Zune vid=0x045e pid=0x0710");

        let usb_err = ZuneDetectError::UsbError("test error".into());
        assert_eq!(format!("{}", usb_err), "USB error: test error");

        let not_found = ZuneDetectError::NotFound {
            hint: "device not found".into(),
        };
        assert_eq!(format!("{}", not_found), "device not found");
    }

    #[test]
    fn zune_backend_capabilities() {
        let backend = ZuneBackend;
        let caps = backend.capabilities();
        assert_eq!(caps.family, DeviceFamily::Zune);
        assert_eq!(caps.supported_formats, &["mp3", "wma", "aac"]);
        assert_eq!(caps.transcode_target, "mp3");
        assert_eq!(caps.music_root, "/Music");
        assert_eq!(caps.max_art_dimensions, Some((200, 200)));
    }
}
