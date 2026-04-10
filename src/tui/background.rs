use std::sync::mpsc;
use std::thread;

use zytunes::device::{
    DeviceBackend, DeviceCapabilities, DeviceFamily, ZuneBackend, ZuneDeviceData,
};
use zytunes::mtp::native::NativeSession;
use zytunes::mtp::parse::DeviceEntry;
use zytunes::mtp::DeviceSession;
use zytunes::{
    collect_photo_files, collect_video_files, make_transcode_temp_dir, needs_transcoding,
    resize_photo_for_zune, transcode_to_mp3,
};

/// Commands sent from the main TUI thread to the background worker.
pub enum BgCommand {
    LoadLibrary {
        xml_path: String,
        music_dir: Option<String>,
    },
    Connect,
    LoadDeviceTracks,
    Disconnect,
    ExecuteSyncQueue(Vec<SyncItem>),
    RemoveFromDevice(Vec<(String, u64)>),
    SyncPhotos {
        dir: String,
    },
    SyncVideos {
        dir: String,
    },
    CancelSync,
    /// Load album art from ID3 tags in the background.
    LoadAlbumArt {
        key: String,
        paths: Vec<String>,
    },
}

/// A single item to sync (resolved to a file path).
#[derive(Clone)]
pub struct SyncItem {
    pub artist: String,
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
    LibraryLoaded(Result<Box<dyn zytunes::library::MusicLibrary + Send>, String>),
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
    StorageUpdated(StorageInfo),
    SyncComplete {
        success: usize,
        failed: usize,
    },
    RemoveProgress {
        current: usize,
        total: usize,
        name: String,
    },
    DeviceTrackAdded(DeviceEntry),
    DeviceTrackRemoved(String),
    PhotoSyncComplete {
        success: usize,
        failed: usize,
    },
    VideoSyncComplete {
        success: usize,
        failed: usize,
    },
    RemoveComplete {
        success: usize,
        failed: usize,
    },
    AcquiredItemsCount(u32),
    /// Parsed sync progress status from MTP vendor op 0x922f.
    DeviceSyncStatus(Option<String>),
    /// Album art loaded from ID3 tags in the background.
    AlbumArtLoaded {
        key: String,
        image: Option<image::DynamicImage>,
    },
}

/// Spawn the background worker thread. Returns a sender for commands.
pub fn spawn(event_tx: mpsc::Sender<BgEvent>) -> mpsc::Sender<BgCommand> {
    let (cmd_tx, cmd_rx) = mpsc::channel::<BgCommand>();

    thread::spawn(move || {
        let mut session: Option<Box<dyn DeviceSession>> = None;
        let mut caps: Option<DeviceCapabilities> = None;

        while let Ok(cmd) = cmd_rx.recv() {
            match cmd {
                BgCommand::LoadLibrary {
                    xml_path,
                    music_dir,
                } => {
                    let result = zytunes::load_library(&xml_path, music_dir.as_deref());
                    let _ = event_tx.send(BgEvent::LibraryLoaded(result));
                }
                BgCommand::Connect => {
                    // Use the backend registry to detect and connect.
                    let _ =
                        event_tx.send(BgEvent::SyncMessage("Scanning USB for device...".into()));

                    let backend = ZuneBackend;
                    let detected = match backend.detect() {
                        Ok(d) => d,
                        Err(e) => {
                            let _ = event_tx
                                .send(BgEvent::SyncMessage(format!("Device not found: {}", e)));
                            let _ = event_tx.send(BgEvent::SessionFailed(e));
                            continue;
                        }
                    };

                    let _ =
                        event_tx.send(BgEvent::SyncMessage(format!("Detected: {}", detected.name)));

                    let backend_caps = backend.capabilities();

                    // Build initial DeviceInfo from detection data.
                    let zune_data = detected.backend_data.downcast_ref::<ZuneDeviceData>();
                    let device_info = DeviceInfo {
                        name: detected.name.clone(),
                        firmware_version: detected.firmware.clone(),
                        serial_number: detected.serial.clone(),
                        usb_mode: zune_data.and_then(|d| d.usb_mode.clone()),
                        manufacturer: None,
                        model: None,
                    };
                    let _ = event_tx.send(BgEvent::DeviceDetected(device_info));

                    let _ = event_tx.send(BgEvent::SyncMessage("MTPZ handshake...".into()));
                    let native_log_tx = event_tx.clone();
                    let native_log = move |msg: &str| {
                        let _ = native_log_tx.send(BgEvent::SyncMessage(msg.to_string()));
                    };

                    // Open session via NativeSession directly so we can do
                    // Zune-specific vendor ops before boxing.
                    let zune_product_id = zune_data.map(|d| d.product_id).unwrap_or(0x0710);
                    let connect_result: Result<Box<dyn DeviceSession>, String> =
                        match NativeSession::open(zune_product_id, &native_log) {
                            Ok(mut s) => {
                                s.set_serial(detected.serial.clone());
                                // Prefer MTP firmware version over USB bcdDevice.
                                let fw = s
                                    .firmware_version
                                    .clone()
                                    .or_else(|| detected.firmware.clone());
                                // Query storage for model detection before boxing.
                                if let Ok((total, free)) = s.get_storage_info() {
                                    let model = zytunes::device::zune_model_from_storage(total);
                                    let used = total.saturating_sub(free);
                                    let pct = if total > 0 {
                                        (used * 100 / total) as u8
                                    } else {
                                        0
                                    };
                                    let _ = event_tx.send(BgEvent::DeviceDetected(DeviceInfo {
                                        name: model.to_string(),
                                        firmware_version: fw,
                                        serial_number: detected.serial.clone(),
                                        usb_mode: zune_data.and_then(|d| d.usb_mode.clone()),
                                        manufacturer: Some("Microsoft".to_string()),
                                        model: Some(model.to_string()),
                                    }));
                                    let _ =
                                        event_tx.send(BgEvent::SessionReady(Some(StorageInfo {
                                            total_bytes: total,
                                            free_bytes: free,
                                            used_bytes: used,
                                            used_percent: pct,
                                        })));
                                }

                                // Zune-specific vendor operations.
                                if detected.family == DeviceFamily::Zune {
                                    // Query acquired items (podcasts, Zune-to-Zune shares).
                                    match s.get_acquired_items_count() {
                                        Ok(count) => {
                                            let _ =
                                                event_tx.send(BgEvent::AcquiredItemsCount(count));
                                        }
                                        Err(e) => {
                                            let _ = event_tx.send(BgEvent::SyncMessage(format!(
                                                "Could not query acquired items: {}",
                                                e
                                            )));
                                        }
                                    }

                                    // Query sync progress (vendor op 0x922f).
                                    let sync_status = match s.get_sync_progress() {
                                        Ok(raw) => Some(parse_sync_progress(&raw)),
                                        Err(e) => {
                                            let _ = event_tx.send(BgEvent::SyncMessage(format!(
                                                "Sync progress query failed: {}",
                                                e
                                            )));
                                            None
                                        }
                                    };
                                    let _ = event_tx.send(BgEvent::DeviceSyncStatus(sync_status));
                                }

                                wire_log_sender(&mut s, &event_tx);
                                let _ = event_tx.send(BgEvent::SyncMessage(
                                    "Connected via native MTP backend".into(),
                                ));
                                Ok(Box::new(s))
                            }
                            Err(e) => Err(e),
                        };

                    match connect_result {
                        Ok(s) => {
                            let _ =
                                event_tx.send(BgEvent::SyncMessage("Session established".into()));

                            session = Some(s);
                            caps = Some(backend_caps);

                            let music_root =
                                caps.as_ref().map(|c| c.music_root).unwrap_or("/Music");

                            // Auto-load device tracks after connection.
                            let _ = event_tx
                                .send(BgEvent::SyncMessage("Loading device library...".into()));
                            let _ = event_tx.send(BgEvent::LoadingDeviceTracks);
                            if let Some(ref mut s) = session {
                                match s.collect_all_tracks(music_root) {
                                    Ok(tracks) => {
                                        let _ = event_tx.send(BgEvent::SyncMessage(format!(
                                            "Loaded {} tracks from device",
                                            tracks.len()
                                        )));
                                        let _ = event_tx.send(BgEvent::DeviceTracksLoaded(tracks));
                                    }
                                    Err(e) => {
                                        let _ = event_tx.send(BgEvent::SyncMessage(format!(
                                            "Failed to load tracks: {}",
                                            e
                                        )));
                                        let _ = event_tx.send(BgEvent::Error(e));
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            let _ = event_tx
                                .send(BgEvent::SyncMessage(format!("Connection failed: {}", e)));
                            let _ = event_tx.send(BgEvent::SessionFailed(e));
                        }
                    }
                }
                BgCommand::LoadDeviceTracks => {
                    if let Some(ref mut s) = session {
                        let music_root = caps.as_ref().map(|c| c.music_root).unwrap_or("/Music");
                        let _ = event_tx
                            .send(BgEvent::SyncMessage("Refreshing device library...".into()));
                        let _ = event_tx.send(BgEvent::LoadingDeviceTracks);
                        match s.collect_all_tracks(music_root) {
                            Ok(tracks) => {
                                let _ = event_tx.send(BgEvent::SyncMessage(format!(
                                    "Loaded {} tracks from device",
                                    tracks.len()
                                )));
                                let _ = event_tx.send(BgEvent::DeviceTracksLoaded(tracks));
                            }
                            Err(e) => {
                                let _ = event_tx
                                    .send(BgEvent::SyncMessage(format!("Refresh failed: {}", e)));
                                let _ = event_tx.send(BgEvent::Error(e));
                            }
                        }
                    } else {
                        let _ = event_tx.send(BgEvent::SyncMessage("No active session".into()));
                        let _ = event_tx.send(BgEvent::Error("No active session".into()));
                    }
                }
                BgCommand::Disconnect => {
                    let _ = event_tx.send(BgEvent::SyncMessage("Disconnected".into()));
                    session = None;
                }
                BgCommand::CancelSync => {
                    // Handled inline during sync execution via try_recv.
                }
                BgCommand::RemoveFromDevice(items) => {
                    if let Some(ref mut s) = session {
                        let total = items.len();
                        let mut success = 0usize;
                        let mut failed = 0usize;

                        let _ = event_tx.send(BgEvent::SyncMessage(format!(
                            "Removing {} tracks from device...",
                            total
                        )));

                        for (i, (path, object_id)) in items.iter().enumerate() {
                            // Check for cancel.
                            if let Ok(BgCommand::CancelSync) = cmd_rx.try_recv() {
                                let _ =
                                    event_tx.send(BgEvent::SyncMessage("Removal cancelled".into()));
                                break;
                            }

                            let name = path.rsplit('/').next().unwrap_or(path).to_string();
                            let _ = event_tx.send(BgEvent::RemoveProgress {
                                current: i + 1,
                                total,
                                name: name.clone(),
                            });

                            // Try delete by object ID first (most reliable),
                            // fall back to path-based rm.
                            let result = if *object_id > 0 {
                                s.rm_by_id(*object_id as u32)
                            } else {
                                s.rm(path)
                            };

                            match result {
                                Ok(()) => {
                                    success += 1;
                                    let _ = event_tx.send(BgEvent::SyncMessage(format!(
                                        "[{}/{}] Removed \"{}\"",
                                        i + 1,
                                        total,
                                        name
                                    )));
                                    let _ =
                                        event_tx.send(BgEvent::DeviceTrackRemoved(path.clone()));
                                }
                                Err(e) => {
                                    failed += 1;
                                    let _ = event_tx.send(BgEvent::SyncMessage(format!(
                                        "[{}/{}] Failed to remove \"{}\": {}",
                                        i + 1,
                                        total,
                                        name,
                                        e
                                    )));
                                }
                            }
                        }

                        let _ = event_tx.send(BgEvent::SyncMessage(format!(
                            "Removal done: {} removed, {} failed",
                            success, failed
                        )));

                        // Clean up empty artist/album folders.
                        if success > 0 {
                            match s.cleanup_empty_folders() {
                                Ok(n) if n > 0 => {
                                    let _ = event_tx.send(BgEvent::SyncMessage(format!(
                                        "Cleaned up {} empty folder(s)",
                                        n
                                    )));
                                }
                                _ => {}
                            }
                        }

                        // Update storage after removal.
                        if let Ok((tot, free)) = s.get_storage_info() {
                            let used = tot.saturating_sub(free);
                            let pct = if tot > 0 { (used * 100 / tot) as u8 } else { 0 };
                            let _ = event_tx.send(BgEvent::StorageUpdated(StorageInfo {
                                total_bytes: tot,
                                free_bytes: free,
                                used_bytes: used,
                                used_percent: pct,
                            }));
                        }

                        let _ = event_tx.send(BgEvent::RemoveComplete { success, failed });
                    } else {
                        let _ = event_tx.send(BgEvent::Error("No active session".into()));
                    }
                }
                BgCommand::SyncPhotos { dir } => {
                    if let Some(ref mut s) = session {
                        let files = collect_photo_files(&[dir.as_str()]);
                        if files.is_empty() {
                            let _ = event_tx
                                .send(BgEvent::SyncMessage("No photos found to sync".into()));
                            let _ = event_tx.send(BgEvent::PhotoSyncComplete {
                                success: 0,
                                failed: 0,
                            });
                            continue;
                        }

                        let existing: std::collections::HashSet<String> = s
                            .ls("/Photos")
                            .unwrap_or_default()
                            .iter()
                            .map(|e| e.name.clone())
                            .collect();

                        let total = files.len();
                        let mut success = 0usize;
                        let mut failed = 0usize;

                        let _ = event_tx
                            .send(BgEvent::SyncMessage(format!("Syncing {} photos...", total)));

                        for (i, file) in files.iter().enumerate() {
                            if let Ok(BgCommand::CancelSync) = cmd_rx.try_recv() {
                                let _ = event_tx
                                    .send(BgEvent::SyncMessage("Photo sync cancelled".into()));
                                break;
                            }

                            let filename = std::path::Path::new(file)
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy();
                            let device_filename = format!(
                                "{}.jpg",
                                std::path::Path::new(&*filename)
                                    .file_stem()
                                    .unwrap_or_default()
                                    .to_string_lossy()
                            );

                            if existing.contains(&device_filename) {
                                let _ = event_tx.send(BgEvent::SyncMessage(format!(
                                    "[{}/{}] {} skipped (on device)",
                                    i + 1,
                                    total,
                                    filename
                                )));
                                continue;
                            }

                            let _ = event_tx.send(BgEvent::SyncMessage(format!(
                                "[{}/{}] Syncing photo: {}",
                                i + 1,
                                total,
                                filename
                            )));

                            match resize_photo_for_zune(file) {
                                Ok(jpeg_data) => {
                                    match s.import_photo(&device_filename, &jpeg_data) {
                                        Ok(_) => success += 1,
                                        Err(e) => {
                                            failed += 1;
                                            let _ = event_tx.send(BgEvent::SyncMessage(format!(
                                                "  FAILED: {}",
                                                e
                                            )));
                                        }
                                    }
                                }
                                Err(e) => {
                                    failed += 1;
                                    let _ = event_tx.send(BgEvent::SyncMessage(format!(
                                        "  FAILED (resize): {}",
                                        e
                                    )));
                                }
                            }
                        }

                        if let Ok((tot, free)) = s.get_storage_info() {
                            let used = tot.saturating_sub(free);
                            let pct = if tot > 0 { (used * 100 / tot) as u8 } else { 0 };
                            let _ = event_tx.send(BgEvent::StorageUpdated(StorageInfo {
                                total_bytes: tot,
                                free_bytes: free,
                                used_bytes: used,
                                used_percent: pct,
                            }));
                        }

                        let _ = event_tx.send(BgEvent::SyncMessage(format!(
                            "Photo sync done: {} synced, {} failed",
                            success, failed
                        )));
                        let _ = event_tx.send(BgEvent::PhotoSyncComplete { success, failed });
                    } else {
                        let _ = event_tx.send(BgEvent::Error("No active session".into()));
                    }
                }
                BgCommand::SyncVideos { dir } => {
                    if let Some(ref mut s) = session {
                        let files = collect_video_files(&[dir.as_str()]);
                        if files.is_empty() {
                            let _ = event_tx
                                .send(BgEvent::SyncMessage("No videos found to sync".into()));
                            let _ = event_tx.send(BgEvent::VideoSyncComplete {
                                success: 0,
                                failed: 0,
                            });
                            continue;
                        }

                        let existing: std::collections::HashSet<String> = s
                            .ls("/Videos")
                            .unwrap_or_default()
                            .iter()
                            .map(|e| e.name.clone())
                            .collect();

                        let total = files.len();
                        let mut success = 0usize;
                        let mut failed = 0usize;

                        let _ = event_tx
                            .send(BgEvent::SyncMessage(format!("Syncing {} videos...", total)));

                        for (i, file) in files.iter().enumerate() {
                            if let Ok(BgCommand::CancelSync) = cmd_rx.try_recv() {
                                let _ = event_tx
                                    .send(BgEvent::SyncMessage("Video sync cancelled".into()));
                                break;
                            }

                            let filename = std::path::Path::new(file)
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .to_string();

                            if existing.contains(&filename) {
                                let _ = event_tx.send(BgEvent::SyncMessage(format!(
                                    "[{}/{}] {} skipped (on device)",
                                    i + 1,
                                    total,
                                    filename
                                )));
                                continue;
                            }

                            let _ = event_tx.send(BgEvent::SyncMessage(format!(
                                "[{}/{}] Syncing video: {}",
                                i + 1,
                                total,
                                filename
                            )));

                            match std::fs::read(file) {
                                Ok(data) => match s.import_video(&filename, &data) {
                                    Ok(_) => success += 1,
                                    Err(e) => {
                                        failed += 1;
                                        let _ = event_tx
                                            .send(BgEvent::SyncMessage(format!("  FAILED: {}", e)));
                                    }
                                },
                                Err(e) => {
                                    failed += 1;
                                    let _ = event_tx.send(BgEvent::SyncMessage(format!(
                                        "  FAILED (read): {}",
                                        e
                                    )));
                                }
                            }
                        }

                        if let Ok((tot, free)) = s.get_storage_info() {
                            let used = tot.saturating_sub(free);
                            let pct = if tot > 0 { (used * 100 / tot) as u8 } else { 0 };
                            let _ = event_tx.send(BgEvent::StorageUpdated(StorageInfo {
                                total_bytes: tot,
                                free_bytes: free,
                                used_bytes: used,
                                used_percent: pct,
                            }));
                        }

                        let _ = event_tx.send(BgEvent::SyncMessage(format!(
                            "Video sync done: {} synced, {} failed",
                            success, failed
                        )));
                        let _ = event_tx.send(BgEvent::VideoSyncComplete { success, failed });
                    } else {
                        let _ = event_tx.send(BgEvent::Error("No active session".into()));
                    }
                }
                BgCommand::ExecuteSyncQueue(items) => {
                    if let Some(ref mut s) = session {
                        let cur_caps = caps.clone();
                        let supported_formats = cur_caps
                            .as_ref()
                            .map(|c| c.supported_formats)
                            .unwrap_or(&["mp3", "wma", "aac"]);
                        let max_art_dims = cur_caps.as_ref().and_then(|c| c.max_art_dimensions);

                        let total = items.len();
                        let mut success = 0usize;
                        let mut failed = 0usize;

                        let temp_dir = make_transcode_temp_dir();
                        let _ = std::fs::create_dir_all(&temp_dir);

                        let to_transcode = items
                            .iter()
                            .filter(|it| needs_transcoding(&it.location, supported_formats))
                            .count();
                        let _ = event_tx.send(BgEvent::SyncMessage(format!(
                            "Starting sync: {} tracks ({} need transcoding)",
                            total, to_transcode
                        )));

                        for (i, item) in items.iter().enumerate() {
                            // Check for cancel command.
                            if let Ok(BgCommand::CancelSync) = cmd_rx.try_recv() {
                                let _ =
                                    event_tx.send(BgEvent::SyncMessage("Sync cancelled".into()));
                                break;
                            }
                            let _ = event_tx.send(BgEvent::SyncProgress {
                                current: i + 1,
                                total,
                                track_name: item.name.clone(),
                            });

                            let upload_path =
                                if needs_transcoding(&item.location, supported_formats) {
                                    let _ = event_tx.send(BgEvent::SyncMessage(format!(
                                        "[{}/{}] Transcoding \"{}\" to MP3...",
                                        i + 1,
                                        total,
                                        item.name
                                    )));
                                    match transcode_to_mp3(&item.location, &temp_dir, max_art_dims)
                                    {
                                        Ok(p) => {
                                            let size =
                                                std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
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

                            match s.import_track(&upload_path) {
                                Ok(id) => {
                                    success += 1;
                                    let _ = event_tx
                                        .send(BgEvent::SyncMessage(format!("  OK (id: {})", id)));
                                    let _ = event_tx.send(BgEvent::SyncTrackDone {
                                        track_name: item.name.clone(),
                                        success: true,
                                        error: None,
                                    });
                                    // Add to device track list in-memory (avoids full rescan).
                                    let file_size = std::fs::metadata(&upload_path)
                                        .map(|m| m.len())
                                        .unwrap_or(0);
                                    let entry = DeviceEntry {
                                        object_id: id,
                                        storage_id: 0,
                                        format: "MP3".to_string(),
                                        size: file_size,
                                        name: format!(
                                            "{}/{}/{}",
                                            item.artist, item.album, item.name
                                        ),
                                    };
                                    let _ = event_tx.send(BgEvent::DeviceTrackAdded(entry));
                                    // Update storage info every 5 tracks (avoid per-track USB overhead).
                                    if (i + 1).is_multiple_of(5) || i + 1 == total {
                                        if let Ok((tot, free)) = s.get_storage_info() {
                                            let used = tot.saturating_sub(free);
                                            let pct =
                                                if tot > 0 { (used * 100 / tot) as u8 } else { 0 };
                                            let _ = event_tx.send(BgEvent::StorageUpdated(
                                                StorageInfo {
                                                    total_bytes: tot,
                                                    free_bytes: free,
                                                    used_bytes: used,
                                                    used_percent: pct,
                                                },
                                            ));
                                        }
                                    }
                                }
                                Err(e) => {
                                    failed += 1;
                                    let _ = event_tx
                                        .send(BgEvent::SyncMessage(format!("  FAILED: {}", e)));
                                    let _ = event_tx.send(BgEvent::SyncTrackDone {
                                        track_name: item.name.clone(),
                                        success: false,
                                        error: Some(e),
                                    });
                                }
                            }
                        }

                        let _ = std::fs::remove_dir_all(&temp_dir);

                        // Save sync progress to disk so the device can skip
                        // re-enumerating already-synced content on next connect.
                        if success > 0 {
                            s.save_sync_progress();
                        }

                        let _ = event_tx.send(BgEvent::SyncMessage(format!(
                            "Done: {} synced, {} failed",
                            success, failed
                        )));
                        let _ = event_tx.send(BgEvent::SyncComplete { success, failed });
                    } else {
                        let _ = event_tx.send(BgEvent::Error("No active session".into()));
                    }
                }
                BgCommand::LoadAlbumArt { key, paths } => {
                    let mut result = None;
                    for path in &paths {
                        if let Ok(tag) = id3::Tag::read_from_path(path) {
                            if let Some(pic) = tag.pictures().next() {
                                if let Ok(img) = image::load_from_memory(&pic.data) {
                                    result = Some(img);
                                    break;
                                }
                            }
                        }
                    }
                    let _ = event_tx.send(BgEvent::AlbumArtLoaded { key, image: result });
                }
            }
        }
    });

    cmd_tx
}

/// Wire up a log-sender channel: creates a channel, calls set_log_sender on the
/// session, and spawns a forwarding thread that relays log messages as BgEvent::SyncMessage.
fn wire_log_sender(session: &mut dyn std::any::Any, event_tx: &mpsc::Sender<BgEvent>) {
    // We need to handle both session types that have set_log_sender.
    if let Some(s) = session.downcast_mut::<NativeSession>() {
        let log_event_tx = event_tx.clone();
        let (log_tx, log_rx) = mpsc::channel::<String>();
        s.set_log_sender(log_tx);
        thread::spawn(move || {
            while let Ok(msg) = log_rx.recv() {
                let _ = log_event_tx.send(BgEvent::SyncMessage(msg));
            }
        });
    }
}

/// Parse the raw sync progress payload from MTP vendor op 0x922f.
/// The 1036-byte struct has a u32 version/status at offset 0; the rest is
/// sync counters and timestamps that are all zeros on a never-synced device.
fn parse_sync_progress(data: &[u8]) -> String {
    if data.len() < 4 {
        return "Unknown".to_string();
    }
    let status = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);

    // Check whether any byte beyond the first 4 is non-zero.
    let has_body = data.len() > 4 && data[4..].iter().any(|&b| b != 0);

    match (status, has_body) {
        (0, _) => "No sync history".to_string(),
        (_, true) => "Sync data available".to_string(),
        (_, false) => "No sync history".to_string(),
    }
}

#[cfg(test)]
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

    #[test]
    fn sync_progress_all_zeros() {
        let data = vec![0u8; 1036];
        assert_eq!(parse_sync_progress(&data), "No sync history");
    }

    #[test]
    fn sync_progress_status_one_body_zeros() {
        let mut data = vec![0u8; 1036];
        data[0..4].copy_from_slice(&1u32.to_le_bytes());
        assert_eq!(parse_sync_progress(&data), "No sync history");
    }

    #[test]
    fn sync_progress_status_one_body_nonzero() {
        let mut data = vec![0u8; 1036];
        data[0..4].copy_from_slice(&1u32.to_le_bytes());
        data[8] = 0x42; // some non-zero byte in the body
        assert_eq!(parse_sync_progress(&data), "Sync data available");
    }

    #[test]
    fn sync_progress_too_short() {
        assert_eq!(parse_sync_progress(&[0, 1]), "Unknown");
    }
}
