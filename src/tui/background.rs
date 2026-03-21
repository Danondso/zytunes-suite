use std::sync::mpsc;
use std::thread;

use zytunes::device::ZuneDevice;
use zytunes::mtp::aft::AftSession;
use zytunes::mtp::parse::DeviceEntry;
use zytunes::mtp::DeviceSession;
use zytunes::{make_transcode_temp_dir, needs_transcoding, transcode_to_mp3};

/// Commands sent from the main TUI thread to the background worker.
pub enum BgCommand {
    LoadLibrary(String),
    Connect,
    LoadDeviceTracks,
    Disconnect,
    ExecuteSyncQueue(Vec<SyncItem>),
    CancelSync,
}

/// A single item to sync (resolved to a file path).
#[derive(Clone)]
pub struct SyncItem {
    #[allow(dead_code)]
    pub artist: String,
    #[allow(dead_code)]
    pub album: String,
    pub name: String,
    pub location: String,
}

/// Device info gathered from USB detection and MTP session.
pub struct DeviceInfo {
    pub name: String,
    pub firmware_version: Option<String>,
    pub serial_number: Option<String>,
    pub usb_mode: Option<String>,
    pub manufacturer: Option<String>,
    pub model: Option<String>,
    pub mtp_version: Option<String>,
}

/// Storage info from the MTP session.
pub struct StorageInfo {
    pub used_bytes: u64,
    pub free_bytes: u64,
    pub total_bytes: u64,
    pub used_percent: u8,
}

/// Events sent from the background worker back to the TUI.
pub enum BgEvent {
    LibraryLoaded(Result<zytunes::library::ItunesLibrary, String>),
    DeviceDetected(DeviceInfo),
    SessionReady(Option<StorageInfo>),
    SessionFailed(String),
    DeviceTracksLoaded(Vec<DeviceEntry>),
    LoadingDeviceTracks,
    Error(String),
    SyncProgress {
        current: usize,
        total: usize,
        track_name: String,
    },
    SyncTrackDone {
        track_name: String,
        success: bool,
        error: Option<String>,
    },
    SyncMessage(String),
    SyncComplete {
        success: usize,
        failed: usize,
        skipped: usize,
    },
}

/// Spawn the background worker thread. Returns a sender for commands.
pub fn spawn(event_tx: mpsc::Sender<BgEvent>) -> mpsc::Sender<BgCommand> {
    let (cmd_tx, cmd_rx) = mpsc::channel::<BgCommand>();

    thread::spawn(move || {
        let mut session: Option<AftSession> = None;

        while let Ok(cmd) = cmd_rx.recv() {
            match cmd {
                BgCommand::LoadLibrary(path) => {
                    let result = zytunes::library::ItunesLibrary::parse(&path);
                    let _ = event_tx.send(BgEvent::LibraryLoaded(result));
                }
                BgCommand::Connect => {
                    // Detect device first via USB.
                    let _ = event_tx.send(BgEvent::SyncMessage(
                        "Scanning USB for Zune...".into(),
                    ));
                    let zune = match ZuneDevice::find() {
                        Ok(z) => z,
                        Err(e) => {
                            let _ = event_tx.send(BgEvent::SyncMessage(format!(
                                "Device not found: {}", e
                            )));
                            let _ = event_tx.send(BgEvent::SessionFailed(format!("{}", e)));
                            continue;
                        }
                    };

                    let dev_name = zune
                        .product_name
                        .clone()
                        .unwrap_or_else(|| "Zune".to_string());
                    let _ = event_tx.send(BgEvent::SyncMessage(format!(
                        "Detected: {}", dev_name
                    )));

                    let device_info = DeviceInfo {
                        name: dev_name,
                        firmware_version: zune.firmware_version.clone(),
                        serial_number: zune.serial_number.clone(),
                        usb_mode: zune.usb_mode.clone(),
                        manufacturer: None,
                        model: None,
                        mtp_version: None,
                    };
                    let _ = event_tx.send(BgEvent::DeviceDetected(device_info));

                    // Open MTP session.
                    let _ = event_tx.send(BgEvent::SyncMessage(
                        "MTPZ handshake...".into(),
                    ));
                    match AftSession::open() {
                        Ok(mut s) => {
                            // Bridge aft-mtp-cli log messages into the TUI log panel.
                            let log_event_tx = event_tx.clone();
                            let (log_tx, log_rx) = mpsc::channel::<String>();
                            s.set_log_sender(log_tx);
                            thread::spawn(move || {
                                while let Ok(msg) = log_rx.recv() {
                                    let _ = log_event_tx.send(BgEvent::SyncMessage(msg));
                                }
                            });

                            let _ = event_tx.send(BgEvent::SyncMessage(
                                "Session established".into(),
                            ));

                            // Query storage info from the MTP session.
                            let storage = query_storage_info(&mut s);
                            if let Some(ref st) = storage {
                                let total_gb = st.total_bytes as f64 / 1_073_741_824.0;
                                let free_gb = st.free_bytes as f64 / 1_073_741_824.0;
                                let _ = event_tx.send(BgEvent::SyncMessage(format!(
                                    "Storage: {:.1} GB free of {:.1} GB",
                                    free_gb, total_gb
                                )));
                            }

                            // Query MTP-level device info to enrich what we got from USB.
                            if let Ok(lines) = s.device_info() {
                                let mut mtp_info = DeviceInfo {
                                    name: zune.product_name.unwrap_or_else(|| "Zune".to_string()),
                                    firmware_version: zune.firmware_version,
                                    serial_number: zune.serial_number,
                                    usb_mode: zune.usb_mode,
                                    manufacturer: lines.first().cloned(),
                                    model: lines.get(1).cloned(),
                                    mtp_version: lines.get(2).cloned(),
                                };
                                // Prefer MTP firmware version if available.
                                if let Some(ref ver) = mtp_info.mtp_version {
                                    if !ver.is_empty() {
                                        mtp_info.firmware_version = Some(ver.clone());
                                    }
                                }
                                // Prefer MTP serial if available.
                                if let Some(ref serial) = lines.get(3) {
                                    if !serial.is_empty() {
                                        mtp_info.serial_number = Some(serial.to_string());
                                    }
                                }
                                // Re-send with enriched info.
                                let _ = event_tx.send(BgEvent::DeviceDetected(mtp_info));
                            }

                            session = Some(s);
                            let _ = event_tx.send(BgEvent::SessionReady(storage));

                            // Auto-load device tracks after connection.
                            let _ = event_tx.send(BgEvent::SyncMessage(
                                "Loading device library...".into(),
                            ));
                            let _ = event_tx.send(BgEvent::LoadingDeviceTracks);
                            if let Some(ref mut s) = session {
                                match s.collect_all_tracks("/Music") {
                                    Ok(tracks) => {
                                        let _ = event_tx.send(BgEvent::SyncMessage(format!(
                                            "Loaded {} tracks from device",
                                            tracks.len()
                                        )));
                                        let _ = event_tx.send(BgEvent::DeviceTracksLoaded(tracks));
                                    }
                                    Err(e) => {
                                        let _ = event_tx.send(BgEvent::SyncMessage(format!(
                                            "Failed to load tracks: {}", e
                                        )));
                                        let _ = event_tx.send(BgEvent::Error(e));
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            let _ = event_tx.send(BgEvent::SyncMessage(format!(
                                "Connection failed: {}", e
                            )));
                            let _ = event_tx.send(BgEvent::SessionFailed(e));
                        }
                    }
                }
                BgCommand::LoadDeviceTracks => {
                    if let Some(ref mut s) = session {
                        let _ = event_tx.send(BgEvent::SyncMessage(
                            "Refreshing device library...".into(),
                        ));
                        let _ = event_tx.send(BgEvent::LoadingDeviceTracks);
                        match s.collect_all_tracks("/Music") {
                            Ok(tracks) => {
                                let _ = event_tx.send(BgEvent::SyncMessage(format!(
                                    "Loaded {} tracks from device",
                                    tracks.len()
                                )));
                                let _ = event_tx.send(BgEvent::DeviceTracksLoaded(tracks));
                            }
                            Err(e) => {
                                let _ = event_tx.send(BgEvent::SyncMessage(format!(
                                    "Refresh failed: {}", e
                                )));
                                let _ = event_tx.send(BgEvent::Error(e));
                            }
                        }
                    } else {
                        let _ = event_tx.send(BgEvent::SyncMessage(
                            "No active session".into(),
                        ));
                        let _ = event_tx.send(BgEvent::Error("No active session".into()));
                    }
                }
                BgCommand::Disconnect => {
                    let _ = event_tx.send(BgEvent::SyncMessage(
                        "Disconnected".into(),
                    ));
                    session = None;
                }
                BgCommand::CancelSync => {
                    // Handled inline during sync execution via try_recv.
                }
                BgCommand::ExecuteSyncQueue(items) => {
                    if let Some(ref mut s) = session {
                        let total = items.len();
                        let mut success = 0usize;
                        let mut failed = 0usize;
                        let skipped = 0usize;

                        let temp_dir = make_transcode_temp_dir();
                        let _ = std::fs::create_dir_all(&temp_dir);

                        let to_transcode = items
                            .iter()
                            .filter(|it| needs_transcoding(&it.location))
                            .count();
                        let _ = event_tx.send(BgEvent::SyncMessage(format!(
                            "Starting sync: {} tracks ({} need transcoding)",
                            total, to_transcode
                        )));

                        for (i, item) in items.iter().enumerate() {
                            // Check for cancel command.
                            if let Ok(BgCommand::CancelSync) = cmd_rx.try_recv() {
                                let _ = event_tx.send(BgEvent::SyncMessage(
                                    "Sync cancelled".into(),
                                ));
                                break;
                            }
                            let _ = event_tx.send(BgEvent::SyncProgress {
                                current: i + 1,
                                total,
                                track_name: item.name.clone(),
                            });

                            let upload_path = if needs_transcoding(&item.location) {
                                let _ = event_tx.send(BgEvent::SyncMessage(format!(
                                    "[{}/{}] Transcoding \"{}\" to MP3...",
                                    i + 1,
                                    total,
                                    item.name
                                )));
                                match transcode_to_mp3(&item.location, &temp_dir) {
                                    Ok(p) => {
                                        let size = std::fs::metadata(&p)
                                            .map(|m| m.len())
                                            .unwrap_or(0);
                                        let _ = event_tx.send(BgEvent::SyncMessage(format!(
                                            "  Transcoded ({:.1} MB)",
                                            size as f64 / 1_048_576.0
                                        )));
                                        p
                                    }
                                    Err(e) => {
                                        failed += 1;
                                        let _ = event_tx.send(BgEvent::SyncMessage(format!(
                                            "  Transcode FAILED: {}",
                                            e
                                        )));
                                        let _ = event_tx.send(BgEvent::SyncTrackDone {
                                            track_name: item.name.clone(),
                                            success: false,
                                            error: Some(e),
                                        });
                                        continue;
                                    }
                                }
                            } else {
                                item.location.clone()
                            };

                            let _ = event_tx.send(BgEvent::SyncMessage(format!(
                                "[{}/{}] Uploading \"{}\"...",
                                i + 1,
                                total,
                                item.name
                            )));

                            match s.zune_import(&upload_path) {
                                Ok(id) => {
                                    success += 1;
                                    let _ = event_tx.send(BgEvent::SyncMessage(format!(
                                        "  OK (id: {})",
                                        id
                                    )));
                                    let _ = event_tx.send(BgEvent::SyncTrackDone {
                                        track_name: item.name.clone(),
                                        success: true,
                                        error: None,
                                    });
                                }
                                Err(e) => {
                                    failed += 1;
                                    let _ = event_tx.send(BgEvent::SyncMessage(format!(
                                        "  FAILED: {}",
                                        e
                                    )));
                                    let _ = event_tx.send(BgEvent::SyncTrackDone {
                                        track_name: item.name.clone(),
                                        success: false,
                                        error: Some(e),
                                    });
                                }
                            }
                        }

                        let _ = std::fs::remove_dir_all(&temp_dir);

                        let _ = event_tx.send(BgEvent::SyncMessage(format!(
                            "Done: {} synced, {} failed",
                            success, failed
                        )));
                        let _ = event_tx.send(BgEvent::SyncComplete {
                            success,
                            failed,
                            skipped,
                        });

                        // Reload device tracks so the count is accurate.
                        let _ = event_tx.send(BgEvent::LoadingDeviceTracks);
                        match s.collect_all_tracks("/Music") {
                            Ok(tracks) => {
                                let _ = event_tx.send(BgEvent::DeviceTracksLoaded(tracks));
                            }
                            Err(e) => {
                                let _ = event_tx.send(BgEvent::Error(
                                    format!("Failed to reload tracks: {}", e),
                                ));
                            }
                        }
                    } else {
                        let _ = event_tx.send(BgEvent::Error("No active session".into()));
                    }
                }
            }
        }
    });

    cmd_tx
}

/// Query storage info from the first storage on the device.
fn query_storage_info(session: &mut AftSession) -> Option<StorageInfo> {
    // List storages first to get a valid storage ID.
    let lines = session.send("storage-list").ok()?;
    // Find first storage line with an ID — format: "65537   volume: ..., description: ..."
    let storage_id = lines.iter().find_map(|line| {
        let trimmed = line.trim();
        let first_token = trimmed.split_whitespace().next()?;
        first_token
            .parse::<u64>()
            .ok()
            .map(|_| first_token.to_string())
    })?;

    let info_lines = session.storage_info(&storage_id).ok()?;

    // Parse: "used 12345678 (45%), free 15000000 bytes of 27345678"
    for line in &info_lines {
        if line.contains("used ") && line.contains("free ") {
            return parse_storage_line(line);
        }
    }
    None
}

fn parse_storage_line(line: &str) -> Option<StorageInfo> {
    // "used 12345678 (45%), free 15000000 bytes of 27345678"
    let parts: Vec<&str> = line.split_whitespace().collect();
    let used_idx = parts.iter().position(|&p| p == "used")?;
    let free_idx = parts.iter().position(|&p| p == "free")?;
    let of_idx = parts.iter().position(|&p| p == "of")?;

    let used_bytes: u64 = parts.get(used_idx + 1)?.parse().ok()?;
    let free_bytes: u64 = parts.get(free_idx + 1)?.parse().ok()?;
    let total_bytes: u64 = parts.get(of_idx + 1)?.parse().ok()?;

    let pct_str = parts.get(used_idx + 2).unwrap_or(&"(0%)");
    let used_percent: u8 = pct_str
        .trim_start_matches('(')
        .trim_end_matches(['%', ')', ','])
        .parse()
        .unwrap_or(0);

    Some(StorageInfo {
        used_bytes,
        free_bytes,
        total_bytes,
        used_percent,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_storage_line_valid() {
        let line = "used 12345678 (45%), free 15000000 bytes of 27345678";
        let info = parse_storage_line(line).unwrap();
        assert_eq!(info.used_bytes, 12345678);
        assert_eq!(info.free_bytes, 15000000);
        assert_eq!(info.total_bytes, 27345678);
        assert_eq!(info.used_percent, 45);
    }

    #[test]
    fn parse_storage_line_zero_percent() {
        let line = "used 0 (0%), free 30000000 bytes of 30000000";
        let info = parse_storage_line(line).unwrap();
        assert_eq!(info.used_bytes, 0);
        assert_eq!(info.used_percent, 0);
    }

    #[test]
    fn parse_storage_line_missing_keyword() {
        assert!(parse_storage_line("garbage data").is_none());
        assert!(parse_storage_line("used 123 free").is_none()); // missing "of"
    }

    #[test]
    fn parse_storage_line_non_numeric() {
        assert!(parse_storage_line("used abc (0%), free 100 bytes of 200").is_none());
    }
}
