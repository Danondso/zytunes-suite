use rusb::{Context, UsbContext};
use std::fmt;

/// Microsoft's USB vendor ID.
const MICROSOFT_VENDOR_ID: u16 = 0x045e;

/// Known Zune product IDs.
///
/// The Zune 30 has appeared under multiple product IDs depending on firmware
/// version and USB mode. This list covers known values; more may exist.
const ZUNE_PRODUCT_IDS: &[(u16, &str)] = &[
    (0x0710, "Zune (media mode)"),
    (0x0711, "Zune (firmware update mode)"),
    (0x0712, "Zune (MTP mode, alternate)"),
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

                // bcdDevice encodes firmware version as BCD (e.g. 0x0310 = 3.10).
                let bcd = desc.device_version();
                let firmware_version = Some(format!(
                    "{}.{:02}",
                    bcd.major(),
                    bcd.minor() * 10 + bcd.sub_minor()
                ));

                return Ok(ZuneDevice {
                    vendor_id: MICROSOFT_VENDOR_ID,
                    product_id,
                    product_name,
                    firmware_version,
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
