use super::{DetectedDevice, DeviceBackend, DeviceCapabilities, DeviceFamily};
use crate::mtp::DeviceSession;
use rusb::UsbContext;

/// Backend-specific data for a detected iPod, stored in `DetectedDevice::backend_data`.
pub struct IpodDeviceData {
    pub mount_point: std::path::PathBuf,
    /// FirewireGuid (USB serial number) used for hash58 database signing.
    /// Required for iPod Classic; `None` for older models that don't check.
    pub firewire_id: Option<String>,
    /// SysInfoExtended `FamilyID`, when the plist is present and parseable.
    pub family_id: Option<u32>,
    /// USB product ID (`05ac:xxxx`) from sysfs/rusb. Used for generation
    /// when SysInfo is empty.
    pub usb_pid: Option<u16>,
    /// SysInfo `boardHwSwInterfaceRev` gestalt.
    pub gestalt: Option<u32>,
    /// SysInfo present but empty (post-2006 firmware).
    pub sysinfo_empty: bool,
    /// Mount filesystem label (`FAT`, `HFS+ (read-only)`, …).
    pub volume_format: Option<String>,
}

impl IpodDeviceData {
    pub fn model_label(&self, model_num: Option<&str>, total_bytes: Option<u64>) -> String {
        super::ipod_model_label(super::IpodModelHints {
            model_num,
            family_id: self.family_id,
            usb_pid: self.usb_pid,
            gestalt: self.gestalt,
            sysinfo_empty: self.sysinfo_empty,
            total_bytes,
        })
    }
}

/// iPod device backend implementing the `DeviceBackend` trait.
///
/// Detects iPods via filesystem mount point scanning (looks for `iPod_Control/`
/// directory). Unlike the Zune backend which uses USB/MTP, the iPod is plain
/// mass storage — all operations are filesystem reads/writes plus iTunesDB
/// serialization.
pub struct IpodBackend;

impl DeviceBackend for IpodBackend {
    fn detect(&self) -> Result<DetectedDevice, String> {
        let detected = ipod_db::detect::detect_ipod().map_err(|e| format!("{}", e))?;

        let usb = read_usb_ipod();
        let usb_pid = usb.as_ref().map(|(pid, _)| *pid);
        let usb_serial = usb.and_then(|(_, serial)| serial);
        #[cfg(target_os = "macos")]
        let firewire_id = read_firewire_id().or(usb_serial);
        #[cfg(not(target_os = "macos"))]
        let firewire_id = usb_serial;

        // Detect already drops generic / automount labels. Unnamed devices
        // use the "iPod" sentinel so the TUI can swap in the composed model.
        let name = detected.name.unwrap_or_else(|| "iPod".into());
        // Keep SysInfo `ModelNumStr` on `DetectedDevice.model` so the
        // connect path can combine it with storage capacity. The TUI
        // DeviceInfo emission maps it to a friendly label.

        Ok(DetectedDevice {
            family: DeviceFamily::Ipod,
            name,
            model: detected.model.clone(),
            serial: detected.serial.clone().or(firewire_id.clone()),
            firmware: detected.firmware_version.clone(),
            backend_data: Box::new(IpodDeviceData {
                mount_point: detected.mount_point,
                firewire_id,
                family_id: detected.family_id,
                usb_pid,
                gestalt: detected.gestalt,
                sysinfo_empty: detected.sysinfo_empty,
                volume_format: detected.volume_format,
            }),
        })
    }

    fn capabilities(&self) -> DeviceCapabilities {
        DeviceCapabilities {
            family: DeviceFamily::Ipod,
            // iPod Classic supports MP3, AAC/M4A, ALAC, WAV, AIFF.
            // We list the ones our transcoder can target or pass through.
            supported_formats: &["mp3", "m4a", "aac", "alac", "wav", "aiff"],
            transcode_target: "mp3",
            // iPod firmware accepts ALAC natively — FLAC library sources
            // transcode to ALAC on push (via ffmpeg) instead of falling
            // through to lossy MP3, preserving the lossless tier.
            lossless_target: Some("alac"),
            music_root: ":iPod_Control:Music",
            max_art_dimensions: None, // Artwork handled via ArtworkDB, not embedded
        }
    }

    fn open_session(
        &self,
        detected: &DetectedDevice,
        log: Option<std::sync::mpsc::Sender<String>>,
    ) -> Result<Box<dyn DeviceSession + Send>, String> {
        let data = detected
            .backend_data
            .downcast_ref::<IpodDeviceData>()
            .ok_or_else(|| "Invalid backend data for iPod".to_string())?;

        let log_fn = |msg: &str| {
            if let Some(ref tx) = log {
                let _ = tx.send(msg.to_string());
            }
        };

        log_fn("Reading iPod database...");

        let db_path = data
            .mount_point
            .join("iPod_Control")
            .join("iTunes")
            .join("iTunesDB");

        let mut db = if db_path.exists() {
            let raw =
                std::fs::read(&db_path).map_err(|e| format!("Failed to read iTunesDB: {}", e))?;
            let parsed = ipod_db::itunesdb::parse(&raw, data.mount_point.clone())
                .map_err(|e| format!("Failed to parse iTunesDB: {}", e))?;
            log_fn(&format!(
                "Loaded {} tracks, {} playlists",
                parsed.tracks.len(),
                parsed.playlists.len()
            ));
            parsed
        } else {
            log_fn("No existing iTunesDB — creating fresh database");
            ipod_db::IpodDatabase::new(data.mount_point.clone())
        };

        // Fold the firmware-written `Play Counts` sidecar into mhit values
        // before exposing tracks. The firmware accumulates plays/skips here
        // between syncs; iTunes folds them into iTunesDB and deletes the
        // file (we mirror that — `IpodSession::flush()` deletes after the
        // next write). Best-effort: a malformed/missing sidecar is logged
        // and ignored so it never blocks connect.
        let pc_path = data
            .mount_point
            .join("iPod_Control")
            .join("iTunes")
            .join("Play Counts");
        if pc_path.exists() {
            match std::fs::read(&pc_path) {
                Ok(bytes) => match ipod_db::play_counts::parse(&bytes) {
                    Ok(entries) => {
                        if ipod_db::play_counts::apply_to_tracks(&mut db.tracks, &entries) {
                            log_fn(&format!(
                                "Folded {} Play Counts entries into iTunesDB",
                                entries.len()
                            ));
                        } else {
                            log_fn(&format!(
                                "Play Counts has {} entries but iTunesDB has {} tracks — skipped merge",
                                entries.len(),
                                db.tracks.len()
                            ));
                        }
                    }
                    Err(e) => log_fn(&format!("Failed to parse Play Counts: {}", e)),
                },
                Err(e) => log_fn(&format!("Failed to read Play Counts: {}", e)),
            }
        }

        // Parse FirewireGuid for hash58 signing.
        let firewire_id = data
            .firewire_id
            .as_ref()
            .map(|id| ipod_db::hash::parse_firewire_id(id))
            .transpose()
            .map_err(|e| format!("Invalid FirewireGuid: {}", e))?;

        let session = crate::mtp::IpodSession::new(db, firewire_id);
        Ok(Box::new(session))
    }
}

/// Read the connected Apple iPod's USB product ID and serial.
///
/// Linux prefers sysfs so we don't have to open a device that mass-storage
/// already claimed. rusb descriptor listing is the fallback (macOS, or
/// sysfs missing).
fn read_usb_ipod() -> Option<(u16, Option<String>)> {
    #[cfg(target_os = "linux")]
    {
        if let Some(found) = read_sysfs_usb_ipod() {
            return Some(found);
        }
    }
    read_rusb_usb_ipod()
}

#[cfg(target_os = "linux")]
fn read_sysfs_usb_ipod() -> Option<(u16, Option<String>)> {
    let root = std::path::Path::new("/sys/bus/usb/devices");
    for ent in std::fs::read_dir(root).ok()? {
        let path = ent.ok()?.path();
        let vid = read_sysfs_hex_u16(&path.join("idVendor"))?;
        if vid != 0x05AC {
            continue;
        }
        let pid = read_sysfs_hex_u16(&path.join("idProduct"))?;
        if !super::ipod_models::ipod_usb_pid_known(pid) {
            continue;
        }
        let serial = std::fs::read_to_string(path.join("serial"))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        return Some((pid, serial));
    }
    None
}

#[cfg(target_os = "linux")]
fn read_sysfs_hex_u16(path: &std::path::Path) -> Option<u16> {
    let s = std::fs::read_to_string(path).ok()?;
    u16::from_str_radix(s.trim(), 16).ok()
}

fn read_rusb_usb_ipod() -> Option<(u16, Option<String>)> {
    let ctx = rusb::Context::new().ok()?;
    for dev in ctx.devices().ok()?.iter() {
        let desc = dev.device_descriptor().ok()?;
        if desc.vendor_id() != 0x05AC {
            continue;
        }
        let pid = desc.product_id();
        if super::ipod_models::ipod_usb_pid_known(pid) {
            return Some((pid, None));
        }
    }
    None
}

/// macOS: FirewireGuid from ioreg. Linux uses the USB serial from sysfs
/// in `read_usb_ipod`.
#[cfg(target_os = "macos")]
fn read_firewire_id() -> Option<String> {
    let output = std::process::Command::new("ioreg")
        .args(["-r", "-c", "IOUSBHostDevice"])
        .output()
        .ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    let mut in_ipod_block = false;
    for line in stdout.lines() {
        if line.contains("iPod@") || line.contains("\"USB Product Name\" = \"iPod\"") {
            in_ipod_block = true;
        }
        if in_ipod_block && line.contains("kUSBSerialNumberString") {
            if let Some(val) = line.split('=').nth(1) {
                let serial = val.trim().trim_matches('"').to_string();
                if !serial.is_empty() {
                    return Some(serial);
                }
            }
        }
        if in_ipod_block && line.starts_with("+-o ") && !line.contains("iPod") {
            in_ipod_block = false;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipod_backend_capabilities() {
        let backend = IpodBackend;
        let caps = backend.capabilities();
        assert_eq!(caps.family, DeviceFamily::Ipod);
        assert!(caps.supported_formats.contains(&"mp3"));
        assert!(caps.supported_formats.contains(&"m4a"));
        assert_eq!(caps.transcode_target, "mp3");
        assert!(caps.max_art_dimensions.is_none());
    }
}
