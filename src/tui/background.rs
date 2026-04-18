use std::sync::mpsc;
use std::thread;

/// True when `err` indicates the MTP session is effectively dead and no
/// further bulk ops on this session will succeed — only a physical replug
/// recovers. Observed cascade modes:
///   - `0xe00002c0` (kIOReturnNoDevice): interface invalidated.
///   - `0xe00002ed` (kIOReturnNotResponding): device stopped answering.
///   - `"retry after ClearPipeStall"`: our transport tried to clear the
///     stall and re-issue the op, and the retry itself failed. That means
///     the pipe reset didn't recover the session.
///   - `"ReadPipe timed out"`: a command's response never came. Once we've
///     written a command and the device doesn't answer within its timeout,
///     subsequent writes reliably cascade into pipe errors.
fn is_device_gone(err: &str) -> bool {
    err.contains("0xe00002c0")
        || err.contains("0xe00002ed")
        || err.contains("retry after ClearPipeStall")
        || err.contains("ReadPipe timed out")
}

use zytunes::device::{
    DeviceBackend, DeviceCapabilities, DeviceFamily, IpodBackend, ZuneBackend, ZuneDeviceData,
};
use zytunes::mtp::native::NativeSession;
use zytunes::mtp::parse::DeviceEntry;
use zytunes::mtp::DeviceSession;
use zytunes::{
    collect_photo_files, collect_video_files, make_transcode_temp_dir, needs_transcoding,
    needs_video_transcoding, resize_photo_for_zune, transcode_and_import_video, transcode_to_mp3,
};

/// Commands sent from the main TUI thread to the background worker.
pub enum BgCommand {
    LoadLibrary {
        music_dir: Option<String>,
    },
    Connect,
    LoadDeviceTracks,
    Disconnect,
    ExecuteSyncQueue(Vec<SyncItem>),
    /// Append more items onto an already-running sync; dropped if no sync is active.
    AppendSyncQueue(Vec<SyncItem>),
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
    pub family: DeviceFamily,
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
    LibraryScanProgress(zytunes::dirlib::ScanProgress),
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
                BgCommand::LoadLibrary { music_dir } => {
                    let result = zytunes::resolve_music_dir(music_dir.as_deref()).and_then(|dir| {
                        let progress_tx = event_tx.clone();
                        zytunes::dirlib::DirectoryLibrary::scan_with_progress(&dir, |p| {
                            let _ = progress_tx.send(BgEvent::LibraryScanProgress(p));
                        })
                        .map(|l| Box::new(l) as Box<dyn zytunes::library::MusicLibrary + Send>)
                    });
                    let _ = event_tx.send(BgEvent::LibraryLoaded(result));
                }
                BgCommand::Connect => {
                    // Try each backend in order until one detects a device.
                    let _ = event_tx.send(BgEvent::SyncMessage("Scanning for device...".into()));

                    let backends: Vec<Box<dyn DeviceBackend>> =
                        vec![Box::new(ZuneBackend), Box::new(IpodBackend)];

                    let mut detected_result: Option<(
                        Box<dyn DeviceBackend>,
                        zytunes::device::DetectedDevice,
                    )> = None;
                    let mut last_err = String::from("No devices found");

                    for backend in backends {
                        match backend.detect() {
                            Ok(d) => {
                                detected_result = Some((backend, d));
                                break;
                            }
                            Err(e) => {
                                last_err = e;
                            }
                        }
                    }

                    let (backend, detected) = match detected_result {
                        Some(pair) => pair,
                        None => {
                            let _ = event_tx.send(BgEvent::SyncMessage(format!(
                                "Device not found: {}",
                                last_err
                            )));
                            let _ = event_tx.send(BgEvent::SessionFailed(last_err));
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
                        family: detected.family,
                    };
                    let _ = event_tx.send(BgEvent::DeviceDetected(device_info));

                    // Open session — Zune uses NativeSession directly for vendor
                    // ops, other backends use the generic open_session() path.
                    let connect_result: Result<Box<dyn DeviceSession>, String> = if detected.family
                        == DeviceFamily::Zune
                    {
                        let _ = event_tx.send(BgEvent::SyncMessage("MTPZ handshake...".into()));
                        let native_log_tx = event_tx.clone();
                        let native_log = move |msg: &str| {
                            let _ = native_log_tx.send(BgEvent::SyncMessage(msg.to_string()));
                        };

                        let zune_product_id = zune_data.map(|d| d.product_id).unwrap_or(0x0710);
                        match NativeSession::open(zune_product_id, &native_log) {
                            Ok(mut s) => {
                                s.set_serial(detected.serial.clone());
                                let fw = s
                                    .firmware_version
                                    .clone()
                                    .or_else(|| detected.firmware.clone());
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
                                        family: DeviceFamily::Zune,
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
                                match s.get_acquired_items_count() {
                                    Ok(Some(count)) => {
                                        let _ = event_tx.send(BgEvent::AcquiredItemsCount(count));
                                    }
                                    Ok(None) => {}
                                    Err(e) => {
                                        let _ = event_tx.send(BgEvent::SyncMessage(format!(
                                            "Could not query acquired items: {}",
                                            e
                                        )));
                                    }
                                }

                                let sync_status = match s.get_sync_progress() {
                                    Ok(Some(raw)) => Some(parse_sync_progress(&raw)),
                                    Ok(None) => None,
                                    Err(e) => {
                                        let _ = event_tx.send(BgEvent::SyncMessage(format!(
                                            "Sync progress query failed: {}",
                                            e
                                        )));
                                        None
                                    }
                                };
                                let _ = event_tx.send(BgEvent::DeviceSyncStatus(sync_status));

                                wire_log_sender(&mut s, &event_tx);
                                let _ = event_tx.send(BgEvent::SyncMessage(
                                    "Connected via native MTP backend".into(),
                                ));
                                Ok(Box::new(s))
                            }
                            Err(e) => Err(e),
                        }
                    } else {
                        // Generic path for iPod and future backends.
                        let log_tx = event_tx.clone();
                        let log_sender = std::sync::mpsc::channel::<String>();
                        // Forward log messages to event channel.
                        let fwd_tx = event_tx.clone();
                        std::thread::spawn(move || {
                            for msg in log_sender.1 {
                                let _ = fwd_tx.send(BgEvent::SyncMessage(msg));
                            }
                        });
                        match backend.open_session(&detected, Some(log_sender.0)) {
                            Ok(mut s) => {
                                // Query storage for UI.
                                if let Ok((total, free)) = s.get_storage_info() {
                                    let used = total.saturating_sub(free);
                                    let pct = if total > 0 {
                                        (used * 100 / total) as u8
                                    } else {
                                        0
                                    };
                                    let _ = event_tx.send(BgEvent::DeviceDetected(DeviceInfo {
                                        name: detected.name.clone(),
                                        firmware_version: detected.firmware.clone(),
                                        serial_number: detected.serial.clone(),
                                        usb_mode: None,
                                        manufacturer: Some("Apple".to_string()),
                                        model: detected.model.clone(),
                                        family: detected.family,
                                    }));
                                    let _ =
                                        event_tx.send(BgEvent::SessionReady(Some(StorageInfo {
                                            total_bytes: total,
                                            free_bytes: free,
                                            used_bytes: used,
                                            used_percent: pct,
                                        })));
                                }
                                let _ =
                                    log_tx.send(BgEvent::SyncMessage("Connected to iPod".into()));
                                Ok(s)
                            }
                            Err(e) => Err(e),
                        }
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

                                // Pre-warm the write-side library mapping so the first
                                // sync doesn't pause to scan existing artist/album folders.
                                if let Err(e) = s.prewarm_library() {
                                    let _ = event_tx.send(BgEvent::SyncMessage(format!(
                                        "Library pre-warm skipped: {}",
                                        e
                                    )));
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
                BgCommand::AppendSyncQueue(_) => {
                    // Appends are consumed by the in-flight sync loop; any that
                    // arrive when no sync is active are silently dropped.
                }
                BgCommand::RemoveFromDevice(items) => {
                    if let Some(ref mut s) = session {
                        let total = items.len();
                        let mut success = 0usize;
                        let mut failed = 0usize;
                        let mut session_dead = false;

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
                                    let gone = is_device_gone(&e);
                                    let _ = event_tx.send(BgEvent::SyncMessage(format!(
                                        "[{}/{}] Failed to remove \"{}\": {}",
                                        i + 1,
                                        total,
                                        name,
                                        e
                                    )));
                                    if gone {
                                        session_dead = true;
                                        let remaining = total - (i + 1);
                                        let _ = event_tx.send(BgEvent::SyncMessage(format!(
                                            "Device disconnected — aborting removal ({} track(s) skipped). Replug and reconnect.",
                                            remaining
                                        )));
                                        break;
                                    }
                                }
                            }
                        }

                        let _ = event_tx.send(BgEvent::SyncMessage(format!(
                            "Removal done: {} removed, {} failed",
                            success, failed
                        )));

                        // Clean up empty artist/album folders. Skip when the
                        // session is dead — the folder walk would just fail
                        // and log another `NoDevice` error.
                        if success > 0 && !session_dead {
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

                        // Update storage after removal (same reason to skip
                        // on a dead session).
                        if !session_dead {
                            if let Ok((tot, free)) = s.get_storage_info() {
                                let used = tot.saturating_sub(free);
                                let pct = if tot > 0 { (used * 100 / tot) as u8 } else { 0 };
                                let _ = event_tx.send(BgEvent::StorageUpdated(StorageInfo {
                                    total_bytes: tot,
                                    free_bytes: free,
                                    used_bytes: used,
                                    used_percent: pct,
                                }));
                                s.refresh_storage_cache(free);
                            }
                        }

                        let _ = event_tx.send(BgEvent::RemoveComplete { success, failed });

                        if session_dead {
                            session = None;
                            let _ = event_tx.send(BgEvent::SessionFailed(
                                "Device disconnected from USB".into(),
                            ));
                        }
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

                        // Check if ffmpeg is needed and available.
                        let needs_ffmpeg = files.iter().any(|f| needs_video_transcoding(f));
                        if needs_ffmpeg && !zytunes::check_ffmpeg_available() {
                            let _ = event_tx.send(BgEvent::Error(
                                "ffmpeg required for video transcoding but not found".into(),
                            ));
                            let _ = event_tx.send(BgEvent::VideoSyncComplete {
                                success: 0,
                                failed: 0,
                            });
                            continue;
                        }

                        let _ = event_tx
                            .send(BgEvent::SyncMessage(format!("Syncing {} videos...", total)));

                        let temp_dir = make_transcode_temp_dir();

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

                            // For non-WMV files, check the transcoded .wmv name.
                            let device_filename = if needs_video_transcoding(file) {
                                let stem = std::path::Path::new(file)
                                    .file_stem()
                                    .unwrap_or_default()
                                    .to_string_lossy();
                                format!("{stem}.wmv")
                            } else {
                                filename.clone()
                            };

                            if existing.contains(&device_filename) {
                                let _ = event_tx.send(BgEvent::SyncMessage(format!(
                                    "[{}/{}] {} skipped (on device)",
                                    i + 1,
                                    total,
                                    filename
                                )));
                                continue;
                            }

                            if needs_video_transcoding(file) {
                                let _ = event_tx.send(BgEvent::SyncMessage(format!(
                                    "[{}/{}] Transcoding video: {}",
                                    i + 1,
                                    total,
                                    filename
                                )));
                            } else {
                                let _ = event_tx.send(BgEvent::SyncMessage(format!(
                                    "[{}/{}] Syncing video: {}",
                                    i + 1,
                                    total,
                                    filename
                                )));
                            }

                            match transcode_and_import_video(s.as_mut(), file, &temp_dir) {
                                Ok(_) => success += 1,
                                Err(e) => {
                                    failed += 1;
                                    let _ = event_tx
                                        .send(BgEvent::SyncMessage(format!("  FAILED: {}", e)));
                                }
                            }
                        }

                        let _ = std::fs::remove_dir_all(&temp_dir);

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

                        // Mutable queue so `AppendSyncQueue` commands received
                        // mid-sync can extend the work in flight.
                        let mut sync_queue: std::collections::VecDeque<SyncItem> = items.into();
                        let mut total = sync_queue.len();
                        let mut processed = 0usize;
                        let mut success = 0usize;
                        let mut failed = 0usize;
                        let mut cancelled = false;
                        let mut session_dead = false;

                        let temp_dir = make_transcode_temp_dir();
                        let _ = std::fs::create_dir_all(&temp_dir);

                        let to_transcode = sync_queue
                            .iter()
                            .filter(|it| needs_transcoding(&it.location, supported_formats))
                            .count();
                        let _ = event_tx.send(BgEvent::SyncMessage(format!(
                            "Starting sync: {} tracks ({} need transcoding)",
                            total, to_transcode
                        )));

                        while let Some(item) = sync_queue.pop_front() {
                            // Drain any pending commands between tracks:
                            // honour cancel, splice appended items onto the end.
                            while let Ok(cmd) = cmd_rx.try_recv() {
                                match cmd {
                                    BgCommand::CancelSync => cancelled = true,
                                    BgCommand::AppendSyncQueue(more) => {
                                        if !more.is_empty() {
                                            let _ = event_tx.send(BgEvent::SyncMessage(format!(
                                                "Queued {} more track(s) during sync",
                                                more.len()
                                            )));
                                            total += more.len();
                                            sync_queue.extend(more);
                                        }
                                    }
                                    // Other commands dropped during sync; the
                                    // TUI doesn't send them while SyncStatus is Running.
                                    _ => {}
                                }
                            }
                            if cancelled {
                                let _ =
                                    event_tx.send(BgEvent::SyncMessage("Sync cancelled".into()));
                                break;
                            }
                            processed += 1;
                            let _ = event_tx.send(BgEvent::SyncProgress {
                                current: processed,
                                total,
                                track_name: item.name.clone(),
                            });

                            let upload_path =
                                if needs_transcoding(&item.location, supported_formats) {
                                    let _ = event_tx.send(BgEvent::SyncMessage(format!(
                                        "[{}/{}] Transcoding \"{}\" to MP3...",
                                        processed, total, item.name
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
                                processed, total, item.name
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
                                        ..Default::default()
                                    };
                                    let _ = event_tx.send(BgEvent::DeviceTrackAdded(entry));
                                    // Update storage info every 5 tracks (avoid per-track USB overhead).
                                    // Also fire on the last pending item so the final usage is fresh.
                                    if processed.is_multiple_of(5) || sync_queue.is_empty() {
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
                                    let gone = is_device_gone(&e);
                                    let _ = event_tx
                                        .send(BgEvent::SyncMessage(format!("  FAILED: {}", e)));
                                    let _ = event_tx.send(BgEvent::SyncTrackDone {
                                        track_name: item.name.clone(),
                                        success: false,
                                        error: Some(e),
                                    });
                                    if gone {
                                        session_dead = true;
                                        let remaining = sync_queue.len();
                                        let _ = event_tx.send(BgEvent::SyncMessage(format!(
                                            "Device disconnected — aborting sync ({} track(s) skipped). Replug and reconnect.",
                                            remaining
                                        )));
                                        break;
                                    }
                                }
                            }
                        }

                        let _ = std::fs::remove_dir_all(&temp_dir);

                        // Save sync progress to disk so the device can skip
                        // re-enumerating already-synced content on next connect.
                        if success > 0 {
                            s.save_sync_progress();
                            // Update the cached free-space header so the next
                            // reconnect doesn't see a large diff and invalidate
                            // the track + library caches.
                            if let Ok((_, free)) = s.get_storage_info() {
                                s.refresh_storage_cache(free);
                            }
                        }

                        let _ = event_tx.send(BgEvent::SyncMessage(format!(
                            "Done: {} synced, {} failed",
                            success, failed
                        )));
                        let _ = event_tx.send(BgEvent::SyncComplete { success, failed });

                        if session_dead {
                            // Drop the now-useless session so subsequent commands
                            // don't try to issue IOKit calls against a torn-down
                            // interface. The TUI flips to Disconnected via
                            // SessionFailed so the user gets a clear signal to
                            // replug and reconnect.
                            session = None;
                            let _ = event_tx.send(BgEvent::SessionFailed(
                                "Device disconnected from USB".into(),
                            ));
                        }
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
    fn is_device_gone_matches_real_error_string() {
        // Shape of real error strings observed in the cascade-to-death logs.
        assert!(is_device_gone(
            "USB error: WritePipe failed: 0xe00002c0 (retry after ClearPipeStall: 0xe00002c0)"
        ));
        assert!(is_device_gone(
            "USB error: ReadPipe failed: 0xe00002c0 (retry after ClearPipeStall: 0xe00002c0)"
        ));
        // NotResponding: device stopped answering (observed after an art timeout).
        assert!(is_device_gone("USB error: WritePipe failed: 0xe00002ed"));
        // Retry-after-stall failed: the suffix alone means recovery failed.
        assert!(is_device_gone(
            "USB error: WritePipe failed: 0xe000404f (retry after ClearPipeStall: 0xe00002ed)"
        ));
        // Read timeout: once a command is in-flight and the response never
        // comes, subsequent writes cascade.
        assert!(is_device_gone("USB error: ReadPipe timed out (30s)"));
        assert!(is_device_gone("USB error: ReadPipe timed out (90s)"));
    }

    #[test]
    fn is_device_gone_ignores_other_usb_errors() {
        // Transient or unrelated errors must NOT trip the bailout.
        // A bare pipe error (no retry suffix) may still recover via our
        // single-shot ClearPipeStall retry — only mark the session dead
        // once the retry itself has been reported as failed.
        assert!(!is_device_gone("USB error: WritePipe failed: 0xe000404f"));
        assert!(!is_device_gone("MTP protocol error: bad response"));
        assert!(!is_device_gone("IO error: file not found"));
    }

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
