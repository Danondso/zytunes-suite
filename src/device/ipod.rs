use super::{DetectedDevice, DeviceBackend, DeviceCapabilities, DeviceFamily};
use crate::mtp::DeviceSession;

/// Backend-specific data for a detected iPod, stored in `DetectedDevice::backend_data`.
pub struct IpodDeviceData {
    pub mount_point: std::path::PathBuf,
    /// FirewireGuid (USB serial number) used for hash58 database signing.
    /// Required for iPod Classic; `None` for older models that don't check.
    pub firewire_id: Option<String>,
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

        // Read the FirewireGuid from the USB serial number.
        // On macOS, ioreg reports it; on Linux, it's in /sys or via lsusb.
        let firewire_id = read_firewire_id();

        let name = "iPod".to_string();
        Ok(DetectedDevice {
            family: DeviceFamily::Ipod,
            name,
            model: detected.model.clone(),
            serial: detected.serial.clone().or(firewire_id.clone()),
            firmware: detected.firmware_version.clone(),
            backend_data: Box::new(IpodDeviceData {
                mount_point: detected.mount_point,
                firewire_id,
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

/// Try to read the iPod's FirewireGuid (USB serial number) from the OS.
///
/// On macOS, queries ioreg for the iPod USB device's serial string.
/// On Linux, could parse /sys/bus/usb/devices or use lsusb (not yet implemented).
fn read_firewire_id() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        // Query ioreg for iPod USB device serial.
        let output = std::process::Command::new("ioreg")
            .args(["-r", "-c", "IOUSBHostDevice"])
            .output()
            .ok()?;
        let stdout = String::from_utf8_lossy(&output.stdout);

        // Look for an iPod device block and extract its serial.
        let mut in_ipod_block = false;
        for line in stdout.lines() {
            if line.contains("iPod@") || line.contains("\"USB Product Name\" = \"iPod\"") {
                in_ipod_block = true;
            }
            if in_ipod_block && line.contains("kUSBSerialNumberString") {
                // Format: |   "kUSBSerialNumberString" = "000A2700215CDB22"
                if let Some(val) = line.split('=').nth(1) {
                    let serial = val.trim().trim_matches('"').to_string();
                    if !serial.is_empty() {
                        return Some(serial);
                    }
                }
            }
            // ioreg blocks end when indentation decreases; reset on next top-level device.
            if in_ipod_block && line.starts_with("+-o ") && !line.contains("iPod") {
                in_ipod_block = false;
            }
        }
        None
    }

    #[cfg(not(target_os = "macos"))]
    {
        // Linux: try reading from /sys or lsusb. For now, fall back to SysInfo.
        None
    }
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
