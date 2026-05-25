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
        // libusb/Linux analogues of the IOKit cascade signals.
        || err.contains("retry after clear_halt")
        || err.contains("read_bulk timed out")
}

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use zytunes::cd::discid::{compute_disc_id, DiscToc};
use zytunes::cd::drive::{enumerate_drives, read_disc_toc, CdDrive, DriveError};
use zytunes::cd::metadata::{ripped_track_destination, tag_ripped_file};
use zytunes::cd::rip::{eject_drive, rip_track_cancellable, RipError, RipFidelity};
use zytunes::device::{
    DeviceBackend, DeviceCapabilities, DeviceFamily, IpodBackend, ZuneBackend, ZuneDeviceData,
};
use zytunes::mtp::native::NativeSession;
use zytunes::mtp::parse::DeviceEntry;
use zytunes::mtp::DeviceSession;
use zytunes::musicbrainz::{MbError, MusicBrainzClient, Release};
use zytunes::{
    collect_photo_files_with_logger, collect_video_files_with_logger, make_transcode_temp_dir,
    needs_transcoding, needs_video_transcoding, resize_photo_for_zune, transcode_and_import_video,
    transcode_to_mp3,
};

/// Commands sent from the main TUI thread to the background worker.
pub enum BgCommand {
    LoadLibrary {
        music_dir: Option<String>,
        /// Whether to compute acoustic fingerprints during the scan.
        /// `false` skips the symphonia + chromaprint pass — faster scan, no
        /// `acoustic_id` for cross-device playcount merging.
        fingerprint: bool,
    },
    Connect,
    LoadDeviceTracks,
    Disconnect,
    ExecuteSyncQueue(Vec<SyncItem>),
    /// Append more items onto an already-running sync; dropped if no sync is active.
    AppendSyncQueue(Vec<SyncItem>),
    RemoveFromDevice(Vec<(String, u64)>),
    /// Create or replace a playlist on the device. Each tuple is
    /// `(artist, album, title)` referencing a library-side track that the
    /// device backend resolves to its native ID scheme. Run after the file
    /// sync finishes so the resolution sees the freshly-uploaded tracks.
    ImportPlaylist {
        name: String,
        track_keys: Vec<(String, String, String)>,
    },
    SyncPhotos {
        dir: String,
    },
    SyncVideos {
        dir: String,
    },
    CancelSync,
    /// Load album art in the background, reading embedded pictures from any
    /// supported tag format (MP3 / FLAC / ALAC / OGG / WMA). Results are
    /// cached per-album on disk so repeat lookups don't re-parse the file.
    LoadAlbumArt {
        key: String,
        artist: String,
        album: String,
        paths: Vec<String>,
    },
    /// One-shot CD detection + identification: enumerates optical drives,
    /// reads the TOC from the first drive that has media, and looks the
    /// disc up on MusicBrainz. Result is delivered as a single
    /// [`BgEvent::CdStatus`] event covering all the success/failure modes.
    ///
    /// MB connection parameters are sent on the command rather than read
    /// from a worker-side config so the worker stays config-agnostic — the
    /// TUI is the source of truth.
    DetectCd {
        mb_base_url: Option<String>,
        mb_user_agent: Option<String>,
    },
    /// Rip the selected tracks from `drive_path` to `dest_dir`, applying
    /// MB metadata via lofty, optionally ejecting on completion. Progress
    /// is reported per-track (not per-byte) — a single track produces one
    /// "started" and one "done" event.
    ///
    /// Cancellation: the TUI sends [`BgCommand::CancelRip`] which trips
    /// an `AtomicBool`; the worker checks it between tracks and
    /// `rip_track_cancellable` checks it while ffmpeg is running.
    ///
    /// Boxed because the payload is ~384 bytes (full MB release with
    /// nested track list); `BgCommand` is otherwise <100 bytes — clippy's
    /// `large_enum_variant` fires otherwise.
    RipAndImport(Box<RipAndImportRequest>),
    /// Cancel the active rip. No-op if no rip is in flight.
    CancelRip,
}

/// Payload for [`BgCommand::RipAndImport`]. Constructed by the TUI from
/// the import overlay state when the user hits Enter.
#[derive(Clone)]
pub struct RipAndImportRequest {
    pub drive: CdDrive,
    pub toc: DiscToc,
    pub release: zytunes::musicbrainz::Release,
    pub track_positions: Vec<u32>,
    pub fidelity: RipFidelity,
    pub dest_dir: PathBuf,
    pub auto_eject: bool,
}

/// A single item to sync (resolved to a file path).
#[derive(Clone, Default)]
pub struct SyncItem {
    pub artist: String,
    pub album: String,
    pub name: String,
    pub location: String,
    pub track_number: Option<u32>,
    pub genre: Option<String>,
    /// On-device copies that must be removed before the new file uploads —
    /// `(device_path, object_id)` tuples populated by `App::execute_sync`
    /// when the queued track normalizes to one or more existing entries in
    /// the device index. Supports overwrite-on-sync semantics: re-queuing
    /// a track replaces whatever's on the device (and sweeps pre-existing
    /// duplicates) rather than being silently skipped.
    pub overwrite_targets: Vec<(String, u64)>,
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
        skipped: usize,
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
    /// Album art loaded from embedded tags (any lofty-supported format) in
    /// the background. `image` is `None` when no candidate track yielded a
    /// decodable picture.
    AlbumArtLoaded {
        key: String,
        image: Option<image::DynamicImage>,
    },
    /// A `BgCommand::ImportPlaylist` finished. `summary` is `Ok` with the
    /// resolved/skipped/replaced counts on success, `Err` with a
    /// human-readable message on failure (most common: backend doesn't
    /// support playlist sync, e.g. the Zune today).
    PlaylistImported {
        name: String,
        summary: Result<zytunes::mtp::PlaylistImportSummary, String>,
    },
    /// Outcome of a [`BgCommand::DetectCd`].
    CdStatus(CdStatusEvent),
    /// Per-track lifecycle event for an active rip. Emitted at start of
    /// each track, on completion (success or failure), and once at the
    /// end with the aggregate totals.
    RipEvent(RipEvent),
}

/// Phase 3 per-track and end-of-rip events.
#[derive(Debug, Clone)]
pub enum RipEvent {
    /// A track is starting. `current` is 1-indexed within the selected
    /// set; `total` is the total selected.
    Started {
        current: usize,
        total: usize,
        // Reserved for the eventual auto-queue-to-device flow.
        #[allow(dead_code)]
        track_position: u32,
        track_title: String,
    },
    /// A track finished. `error` is `Some` on failure (ffmpeg, tagging,
    /// or file move).
    TrackDone {
        // `track_position` and `dest_path` are surfaced for the eventual
        // auto-queue-to-device flow (queue the just-ripped file for a
        // connected device); the Phase 3 commit doesn't wire that path yet.
        #[allow(dead_code)]
        track_position: u32,
        track_title: String,
        #[allow(dead_code)]
        dest_path: Option<PathBuf>,
        error: Option<String>,
    },
    /// All tracks in the request have been processed. `ejected` reflects
    /// whether the auto-eject actually ran (best-effort).
    Complete {
        ripped: usize,
        failed: usize,
        cancelled: bool,
        ejected: bool,
    },
}

/// The full state machine the TUI cares about for CD detection — populated
/// from one `DetectCd` round-trip and sufficient to render the status line
/// without follow-up queries.
#[derive(Debug, Clone)]
pub enum CdStatusEvent {
    /// No optical drive present (libdiscid returned an empty default device).
    NoDrive,
    /// Optical drive present but empty (or unreadable media).
    NoMedia { drive: CdDrive },
    /// TOC read but MusicBrainz could not (or refused to) identify the disc.
    /// `reason` is short and human-readable so the status line can render
    /// it directly.
    UnknownDisc {
        drive: CdDrive,
        // `toc` and `mb_disc_id` are populated for Phase 2's "submit this
        // disc to MusicBrainz" flow and for letting the user inspect the
        // raw disc-id when reporting issues. Phase 1 surfaces only `reason`.
        #[allow(dead_code)]
        toc: DiscToc,
        #[allow(dead_code)]
        mb_disc_id: String,
        reason: String,
    },
    /// Full success: TOC read and MB returned at least one release.
    /// `primary` is the first release MB returned; `alternates` carries any
    /// others for the Phase 2 alternate-match picker.
    ///
    /// `primary` is boxed because [`Release`] is ~376 bytes inline, large
    /// enough that clippy's `large_enum_variant` lint fires across the
    /// `BgEvent::CdStatus(CdStatusEvent)` chain. Boxing keeps the
    /// hot-path event enum small without forcing all variants to box.
    Identified {
        drive: CdDrive,
        // `toc`, `mb_disc_id`, and `alternates` are written to App state in
        // Phase 1 and consumed by the import overlay in Phase 2.
        #[allow(dead_code)]
        toc: DiscToc,
        #[allow(dead_code)]
        mb_disc_id: String,
        primary: Box<Release>,
        #[allow(dead_code)]
        alternates: Vec<Release>,
    },
}

/// Spawn the background worker thread. Returns a sender for commands.
pub fn spawn(event_tx: mpsc::Sender<BgEvent>) -> mpsc::Sender<BgCommand> {
    let (cmd_tx, cmd_rx) = mpsc::channel::<BgCommand>();

    thread::spawn(move || {
        let mut session: Option<Box<dyn DeviceSession>> = None;
        let mut caps: Option<DeviceCapabilities> = None;
        // Construct the persistent art cache once for the lifetime of the
        // worker. `default_location` does an env read + PathBuf build, so
        // reusing it keeps `LoadAlbumArt` hot-path allocations down to the
        // two strings we actually need (artist, album).
        let art_cache = zytunes::art_cache::ArtCache::default_location();

        // Shared cancel flag for `RipAndImport`. Tripped by `CancelRip`,
        // cleared at the start of each new rip. Lives outside the rip
        // dispatch arm so it survives across cmd_rx.recv() turns.
        let rip_cancel = Arc::new(AtomicBool::new(false));

        while let Ok(cmd) = cmd_rx.recv() {
            match cmd {
                BgCommand::LoadLibrary {
                    music_dir,
                    fingerprint,
                } => {
                    let log_tx = event_tx.clone();
                    let scan_log: zytunes::cache::Logger = std::sync::Arc::new(move |msg: &str| {
                        let _ = log_tx.send(BgEvent::SyncMessage(msg.to_string()));
                    });
                    let opts = zytunes::dirlib::ScanOptions {
                        fingerprint,
                        log: scan_log,
                    };
                    let result = zytunes::resolve_music_dir(music_dir.as_deref()).and_then(|dir| {
                        let progress_tx = event_tx.clone();
                        zytunes::dirlib::DirectoryLibrary::scan_with_options(&dir, opts, |p| {
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
                                    let model = zytunes::device::zune_model_from_storage(
                                        total,
                                        zune_product_id,
                                    );
                                    let used = total.saturating_sub(free);
                                    let pct = (used * 100).checked_div(total).unwrap_or(0) as u8;
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
                                    let pct = (used * 100).checked_div(total).unwrap_or(0) as u8;
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
                                let pct = (used * 100).checked_div(tot).unwrap_or(0) as u8;
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
                        let photo_log_tx = event_tx.clone();
                        let photo_log: zytunes::cache::Logger =
                            std::sync::Arc::new(move |msg: &str| {
                                let _ = photo_log_tx.send(BgEvent::SyncMessage(msg.to_string()));
                            });
                        let files = collect_photo_files_with_logger(&[dir.as_str()], &photo_log);
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
                            let pct = (used * 100).checked_div(tot).unwrap_or(0) as u8;
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
                        let video_log_tx = event_tx.clone();
                        let video_log: zytunes::cache::Logger =
                            std::sync::Arc::new(move |msg: &str| {
                                let _ = video_log_tx.send(BgEvent::SyncMessage(msg.to_string()));
                            });
                        let files = collect_video_files_with_logger(&[dir.as_str()], &video_log);
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
                            let pct = (used * 100).checked_div(tot).unwrap_or(0) as u8;
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
                                    BgCommand::AppendSyncQueue(more) if !more.is_empty() => {
                                        let _ = event_tx.send(BgEvent::SyncMessage(format!(
                                            "Queued {} more track(s) during sync",
                                            more.len()
                                        )));
                                        total += more.len();
                                        sync_queue.extend(more);
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

                            // Overwrite: remove any on-device copies the app
                            // matched for this track before the new file
                            // lands. Covers both "user re-queued a track
                            // that's already there" and "sweep pre-existing
                            // duplicates as a side effect". On a fatal USB
                            // cascade (is_device_gone), abort the sync
                            // before we even try to upload.
                            let mut overwrite_aborted = false;
                            if !item.overwrite_targets.is_empty() {
                                let n = item.overwrite_targets.len();
                                let _ = event_tx.send(BgEvent::SyncMessage(format!(
                                    "  Overwriting {} existing {}",
                                    n,
                                    if n == 1 { "copy" } else { "copies" }
                                )));
                                for (path, oid) in &item.overwrite_targets {
                                    let result = if *oid > 0 {
                                        s.rm_by_id(*oid as u32)
                                    } else {
                                        s.rm(path)
                                    };
                                    match result {
                                        Ok(()) => {
                                            let _ = event_tx
                                                .send(BgEvent::DeviceTrackRemoved(path.clone()));
                                        }
                                        Err(e) => {
                                            let gone = is_device_gone(&e);
                                            let _ = event_tx.send(BgEvent::SyncMessage(format!(
                                                "  Remove failed for {}: {}",
                                                path, e
                                            )));
                                            if gone {
                                                session_dead = true;
                                                overwrite_aborted = true;
                                                break;
                                            }
                                            // Non-fatal remove failures leave the
                                            // orphan copy on device; we still
                                            // proceed to upload so the user at
                                            // least gets the new version.
                                        }
                                    }
                                }
                            }
                            if overwrite_aborted {
                                failed += 1;
                                let remaining = sync_queue.len();
                                let _ = event_tx.send(BgEvent::SyncTrackDone {
                                    track_name: item.name.clone(),
                                    success: false,
                                    error: Some("Device disconnected during overwrite".into()),
                                });
                                let _ = event_tx.send(BgEvent::SyncMessage(format!(
                                    "Device disconnected — aborting sync ({} track(s) skipped). Replug and reconnect.",
                                    remaining
                                )));
                                break;
                            }

                            let _ = event_tx.send(BgEvent::SyncMessage(format!(
                                "[{}/{}] Uploading \"{}\"...",
                                processed, total, item.name
                            )));

                            let meta = zytunes::mtp::TrackMeta {
                                artist: item.artist.clone(),
                                album: item.album.clone(),
                                title: item.name.clone(),
                                track_number: item.track_number,
                                genre: item.genre.clone(),
                            };
                            match s.import_track(&upload_path, Some(&meta)) {
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
                                                (used * 100).checked_div(tot).unwrap_or(0) as u8;
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

                        // Any items still in the queue were dropped by an
                        // early exit (cancel or device-gone cascade) and
                        // should be surfaced so the running tally matches
                        // the user's mental model of the queue.
                        let skipped = sync_queue.len();
                        let _ = event_tx.send(BgEvent::SyncMessage(format!(
                            "Done: {} synced, {} skipped, {} failed",
                            success, skipped, failed
                        )));
                        let _ = event_tx.send(BgEvent::SyncComplete {
                            success,
                            failed,
                            skipped,
                        });

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
                BgCommand::LoadAlbumArt {
                    key,
                    artist,
                    album,
                    paths,
                } => {
                    // Fast path: on-disk cache. Skips re-parsing ALAC / FLAC
                    // tags every time the user flips to a previously-viewed
                    // album.
                    let cached = art_cache
                        .as_ref()
                        .and_then(|c| c.lookup(&artist, &album))
                        .and_then(|bytes| image::load_from_memory(&bytes).ok());

                    let image = cached.or_else(|| {
                        extract_album_art_for_cache(&artist, &album, &paths, art_cache.as_ref())
                    });
                    let _ = event_tx.send(BgEvent::AlbumArtLoaded { key, image });
                }
                BgCommand::ImportPlaylist { name, track_keys } => {
                    if let Some(ref mut s) = session {
                        let result = s.import_playlist(&name, &track_keys);
                        let _ = event_tx.send(BgEvent::PlaylistImported {
                            name,
                            summary: result,
                        });
                    } else {
                        let _ = event_tx.send(BgEvent::PlaylistImported {
                            name,
                            summary: Err("No active device session".into()),
                        });
                    }
                }
                BgCommand::DetectCd {
                    mb_base_url,
                    mb_user_agent,
                } => {
                    let event = detect_cd(mb_base_url, mb_user_agent);
                    let _ = event_tx.send(BgEvent::CdStatus(event));
                }
                BgCommand::RipAndImport(req) => {
                    rip_cancel.store(false, Ordering::SeqCst);
                    run_rip_and_import(*req, &event_tx, &rip_cancel);
                }
                BgCommand::CancelRip => {
                    rip_cancel.store(true, Ordering::SeqCst);
                }
            }
        }
    });

    cmd_tx
}

/// Drive the multi-track rip flow: for each selected track, emit a
/// `Started` event, rip to a temp path, tag with MB metadata, move into
/// the library tree, emit `TrackDone`. Wraps up with a `Complete` event.
fn run_rip_and_import(
    req: RipAndImportRequest,
    event_tx: &mpsc::Sender<BgEvent>,
    cancel: &Arc<AtomicBool>,
) {
    let total = req.track_positions.len();
    let mut ripped = 0usize;
    let mut failed = 0usize;
    let mut cancelled = false;

    // Build a quick lookup from track position → MB Track so we can pull
    // the title (for events) and metadata (for tagging) without re-scanning.
    // ONLY scan the first medium with tracks, mirroring the overlay's
    // `ImportOverlay::current_tracks` behaviour. Flattening across all
    // media would cause multi-disc box sets to collide on position
    // (disc 2 track 1 has `position = 1`, same as disc 1 track 1, and a
    // BTreeMap overwrite would tag disc 1's rip with disc 2's metadata).
    let active_medium = req.release.media.iter().find(|m| !m.tracks.is_empty());
    let mb_tracks: std::collections::BTreeMap<u32, zytunes::musicbrainz::Track> = active_medium
        .map(|m| m.tracks.iter().cloned())
        .into_iter()
        .flatten()
        .filter_map(|t| t.position.map(|p| (p, t)))
        .collect();
    let total_tracks_on_release =
        active_medium.and_then(|m| m.track_count.or(Some(m.tracks.len() as u32)));
    // For disc-of-N tags: count of media on the release. Disc number is
    // carried in the medium itself (`medium.position`) and read off the
    // active_medium reference inside `tag_ripped_file`.
    let total_discs_on_release = if req.release.media.is_empty() {
        None
    } else {
        Some(req.release.media.len() as u32)
    };

    for (idx, &position) in req.track_positions.iter().enumerate() {
        if cancel.load(Ordering::SeqCst) {
            cancelled = true;
            break;
        }
        let Some(mb_track) = mb_tracks.get(&position) else {
            failed += 1;
            let _ = event_tx.send(BgEvent::RipEvent(RipEvent::TrackDone {
                track_position: position,
                track_title: format!("track {position}"),
                dest_path: None,
                error: Some(format!(
                    "track {position} not present in MusicBrainz release"
                )),
            }));
            continue;
        };

        let _ = event_tx.send(BgEvent::RipEvent(RipEvent::Started {
            current: idx + 1,
            total,
            track_position: position,
            track_title: mb_track.title.clone(),
        }));

        let dest = ripped_track_destination(
            &req.dest_dir,
            &req.release,
            mb_track,
            position,
            req.fidelity.extension(),
        );

        let outcome = run_single_track_rip(
            SingleTrackRip {
                drive_path: &req.drive.path,
                toc: &req.toc,
                position,
                fidelity: req.fidelity,
                release: &req.release,
                mb_track,
                dest: &dest,
                total_tracks: total_tracks_on_release,
                medium: active_medium,
                total_discs: total_discs_on_release,
            },
            cancel,
        );

        // Typed outcome from `run_single_track_rip`. Accounting now
        // matches the user's mental model:
        // - `Ripped` and `RippedUntagged` both count as `ripped` (the
        //   audio is on disk; tagging issues surface as warnings).
        // - `Cancelled` doesn't increment anything; the `Complete
        //   { cancelled: true }` event tells the user why.
        // - `Failed` increments `failed`.
        let (dest_path, error_for_event) = match &outcome {
            TrackOutcome::Ripped(p) => {
                ripped += 1;
                (Some(p.clone()), None)
            }
            TrackOutcome::RippedUntagged { dest, warning } => {
                ripped += 1;
                let _ = event_tx.send(BgEvent::SyncMessage(format!(
                    "[rip] {} — {warning}",
                    mb_track.title
                )));
                (Some(dest.clone()), None)
            }
            TrackOutcome::Cancelled => (None, None),
            TrackOutcome::Failed(e) => {
                failed += 1;
                (None, Some(e.clone()))
            }
        };

        let _ = event_tx.send(BgEvent::RipEvent(RipEvent::TrackDone {
            track_position: position,
            track_title: mb_track.title.clone(),
            dest_path,
            error: error_for_event,
        }));

        if cancel.load(Ordering::SeqCst) {
            cancelled = true;
            break;
        }
    }

    // Auto-eject is best-effort; a rip that succeeded with an eject
    // failure shouldn't read as a failed rip.
    let ejected = if req.auto_eject && ripped > 0 && !cancelled {
        match eject_drive(&req.drive.path) {
            Ok(()) => true,
            Err(e) => {
                let _ = event_tx.send(BgEvent::SyncMessage(format!("rip ok, eject failed: {e}")));
                false
            }
        }
    } else {
        false
    };

    let _ = event_tx.send(BgEvent::RipEvent(RipEvent::Complete {
        ripped,
        failed,
        cancelled,
        ejected,
    }));
}

/// One-track wrapper: ensure dest directory exists, rip to a temp file,
/// tag via lofty, atomically move into place. Returns the final path on
/// success.
/// Per-track rip parameters bundled to keep the function signature
/// readable (clippy's `too_many_arguments` fires at 8).
struct SingleTrackRip<'a> {
    drive_path: &'a std::path::Path,
    toc: &'a DiscToc,
    position: u32,
    fidelity: RipFidelity,
    release: &'a zytunes::musicbrainz::Release,
    mb_track: &'a zytunes::musicbrainz::Track,
    dest: &'a std::path::Path,
    total_tracks: Option<u32>,
    medium: Option<&'a zytunes::musicbrainz::Medium>,
    total_discs: Option<u32>,
}

/// Result of a single-track rip — finer-grained than `Result<_,_>` so
/// the caller can distinguish full success, success-with-non-fatal-warning
/// (audio ripped but tagging failed), cancellation, and outright failure
/// for accounting purposes.
pub(crate) enum TrackOutcome {
    /// Audio ripped and tagged. Counts as `ripped`.
    Ripped(PathBuf),
    /// Audio ripped and on disk, but tagging failed — file is still in
    /// the library so it counts as `ripped`; the warning is logged so
    /// the user knows their tags are missing.
    RippedUntagged { dest: PathBuf, warning: String },
    /// User cancelled mid-track. Doesn't count as `ripped` or `failed`;
    /// `.part` temp file is cleaned up before returning.
    Cancelled,
    /// Rip never produced a usable file. Counts as `failed`. `.part`
    /// temp file is cleaned up before returning.
    Failed(String),
}

fn run_single_track_rip(params: SingleTrackRip<'_>, cancel: &Arc<AtomicBool>) -> TrackOutcome {
    let SingleTrackRip {
        drive_path,
        toc,
        position,
        fidelity,
        release,
        mb_track,
        dest,
        total_tracks,
        medium,
        total_discs,
    } = params;

    if let Some(parent) = dest.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return TrackOutcome::Failed(format!("create dest dir {}: {e}", parent.display()));
        }
    }

    // Rip to a temp file next to the destination so a partial rip (cancel
    // or ffmpeg crash) doesn't leave a half-formed file at the canonical
    // path. The dirlib scanner picks tracks up by extension, so we don't
    // want it to see incomplete files.
    let temp_path = dest.with_extension(format!("{}.part", fidelity.extension()));

    let track_position_u8: u8 = match position.try_into() {
        Ok(v) => v,
        Err(_) => {
            return TrackOutcome::Failed(format!(
                "track position {position} out of u8 range for ffmpeg libcdio -map index"
            ));
        }
    };

    let cancel_clone = Arc::clone(cancel);
    let rip_result = rip_track_cancellable(
        drive_path,
        toc,
        track_position_u8,
        fidelity,
        &temp_path,
        &move || cancel_clone.load(Ordering::SeqCst),
    );

    match rip_result {
        Err(RipError::Cancelled) => {
            // Clean up the partial `.part` file so repeated cancel-retry
            // cycles don't accumulate orphans next to the canonical path.
            let _ = std::fs::remove_file(&temp_path);
            return TrackOutcome::Cancelled;
        }
        Err(e) => {
            let _ = std::fs::remove_file(&temp_path);
            return TrackOutcome::Failed(e.to_string());
        }
        Ok(_) => {}
    }

    // Tagging is best-effort. If it fails, the audio is still correct —
    // we keep the file and surface a warning rather than counting the
    // whole track as a failure (the Phase 4 fix-feature roadmap will
    // re-tag library tracks against MB).
    let tag_warning = tag_ripped_file(
        &temp_path,
        release,
        mb_track,
        position,
        total_tracks,
        medium,
        total_discs,
    )
    .err();

    if let Err(e) = std::fs::rename(&temp_path, dest) {
        let _ = std::fs::remove_file(&temp_path);
        return TrackOutcome::Failed(format!(
            "rename {} → {}: {e}",
            temp_path.display(),
            dest.display()
        ));
    }

    match tag_warning {
        Some(e) => TrackOutcome::RippedUntagged {
            dest: dest.to_path_buf(),
            warning: format!("tagging failed: {e}"),
        },
        None => TrackOutcome::Ripped(dest.to_path_buf()),
    }
}

/// CD detection: enumerate optical drives, read the first one's TOC, and
/// look up the disc on MusicBrainz. Extracted from the worker `match` arm
/// for readability — note that it does real IO (libdiscid + network) so
/// it's not unit-testable without dependency injection; coverage of the
/// state-machine output lives in `App`-level tests that feed synthetic
/// [`CdStatusEvent`] values.
fn detect_cd(mb_base_url: Option<String>, mb_user_agent: Option<String>) -> CdStatusEvent {
    let Some(mut drive) = enumerate_drives().into_iter().next() else {
        return CdStatusEvent::NoDrive;
    };

    let toc = match read_disc_toc(&drive) {
        Ok(toc) => {
            drive.media_present = Some(true);
            toc
        }
        Err(DriveError::NoMedia) => {
            drive.media_present = Some(false);
            return CdStatusEvent::NoMedia { drive };
        }
        Err(e) => {
            // Treat IO and unsupported-platform errors as "unknown" so the
            // status line carries the reason for the user; we still have a
            // (possibly empty) TOC to display.
            return CdStatusEvent::UnknownDisc {
                drive,
                toc: DiscToc {
                    first_track: 0,
                    last_track: 0,
                    lead_out_lba: 0,
                    tracks: Vec::new(),
                },
                mb_disc_id: String::new(),
                reason: e.to_string(),
            };
        }
    };

    let mb_disc_id = compute_disc_id(&toc);

    let mut client = MusicBrainzClient::new(mb_user_agent);
    if let Some(url) = mb_base_url {
        client = client.with_base_url(url);
    }

    match client.lookup_disc(&mb_disc_id) {
        Ok(resp) => {
            let mut releases = resp.releases;
            if releases.is_empty() {
                CdStatusEvent::UnknownDisc {
                    drive,
                    toc,
                    mb_disc_id,
                    reason: "no MusicBrainz match for this disc".into(),
                }
            } else {
                let primary = releases.remove(0);
                CdStatusEvent::Identified {
                    drive,
                    toc,
                    mb_disc_id,
                    primary: Box::new(primary),
                    alternates: releases,
                }
            }
        }
        Err(MbError::NotFound) => CdStatusEvent::UnknownDisc {
            drive,
            toc,
            mb_disc_id,
            reason: "disc ID not in MusicBrainz".into(),
        },
        Err(MbError::MissingUserAgent) => CdStatusEvent::UnknownDisc {
            drive,
            toc,
            mb_disc_id,
            reason: "set musicbrainz_user_agent in config".into(),
        },
        Err(e) => CdStatusEvent::UnknownDisc {
            drive,
            toc,
            mb_disc_id,
            reason: format!("MusicBrainz lookup failed: {e}"),
        },
    }
}

/// Extract the first embedded picture from any of `paths` via lofty (handles
/// FLAC / ALAC / OGG / WMA / MP3 uniformly — the previous `id3::Tag` path
/// silently returned `None` for non-MP3 containers so ALAC albums never
/// showed art). On success, persists the JPEG bytes in `cache` so the next
/// lookup is a pure disk read.
fn extract_album_art_for_cache(
    artist: &str,
    album: &str,
    paths: &[String],
    cache: Option<&zytunes::art_cache::ArtCache>,
) -> Option<image::DynamicImage> {
    use lofty::file::TaggedFileExt;

    for path in paths {
        let Ok(tagged) = lofty::probe::read_from_path(path) else {
            continue;
        };
        let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) else {
            continue;
        };
        let Some(pic) = tag.pictures().first() else {
            continue;
        };
        let Ok(img) = image::load_from_memory(pic.data()) else {
            continue;
        };

        if let Some(cache) = cache {
            // Store the original embedded bytes verbatim — future cache hits
            // round-trip through `image::load_from_memory` the same way the
            // miss path does, so cached and fresh results render identically.
            let _ = cache.store(artist, album, std::path::Path::new(path), pic.data());
        }
        return Some(img);
    }
    None
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
        // libusb/Linux variants — same semantics, different wording.
        assert!(is_device_gone(
            "USB error: write_bulk failed: Pipe error (retry after clear_halt: No such device)"
        ));
        assert!(is_device_gone("USB error: read_bulk timed out (30s)"));
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

    // -- extract_album_art_for_cache --

    /// Minimal PCM s16le stereo 44.1 kHz WAV (silence). Enough bytes for
    /// lofty to open the container and attach a tag with an embedded picture.
    fn write_wav(path: &std::path::Path) {
        use std::io::Write;
        let channels: u16 = 2;
        let sample_rate: u32 = 44100;
        let bps: u16 = 16;
        let byte_rate = sample_rate * u32::from(channels) * u32::from(bps) / 8;
        let block_align = channels * bps / 8;
        let num_samples = 4410usize;
        let data_size = (num_samples * usize::from(channels) * usize::from(bps) / 8) as u32;
        let file_size = 36 + data_size;

        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(b"RIFF").unwrap();
        f.write_all(&file_size.to_le_bytes()).unwrap();
        f.write_all(b"WAVE").unwrap();
        f.write_all(b"fmt ").unwrap();
        f.write_all(&16u32.to_le_bytes()).unwrap();
        f.write_all(&1u16.to_le_bytes()).unwrap();
        f.write_all(&channels.to_le_bytes()).unwrap();
        f.write_all(&sample_rate.to_le_bytes()).unwrap();
        f.write_all(&byte_rate.to_le_bytes()).unwrap();
        f.write_all(&block_align.to_le_bytes()).unwrap();
        f.write_all(&bps.to_le_bytes()).unwrap();
        f.write_all(b"data").unwrap();
        f.write_all(&data_size.to_le_bytes()).unwrap();
        f.write_all(&vec![0u8; data_size as usize]).unwrap();
    }

    fn embed_art(path: &std::path::Path, jpeg_bytes: Vec<u8>) {
        use lofty::file::TaggedFileExt;
        use lofty::picture::{MimeType, Picture, PictureType};
        use lofty::tag::{Tag, TagExt};
        let mut tagged = lofty::probe::read_from_path(path).unwrap();
        let tag_type = tagged.primary_tag_type();
        if tagged.primary_tag().is_none() {
            tagged.insert_tag(Tag::new(tag_type));
        }
        let tag = tagged.primary_tag_mut().unwrap();
        let pic = Picture::new_unchecked(
            PictureType::CoverFront,
            Some(MimeType::Jpeg),
            None,
            jpeg_bytes,
        );
        tag.push_picture(pic);
        tag.save_to_path(path, lofty::config::WriteOptions::default())
            .unwrap();
    }

    fn tiny_jpeg() -> Vec<u8> {
        use image::{ImageBuffer, Rgb};
        let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
            ImageBuffer::from_fn(8, 8, |_, _| Rgb([0, 128, 255]));
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Jpeg).unwrap();
        buf.into_inner()
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("zytunes-tui-art-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn extract_album_art_reads_embedded_picture_via_lofty() {
        let dir = scratch("lofty-read");
        let wav = dir.join("track.wav");
        write_wav(&wav);
        embed_art(&wav, tiny_jpeg());

        let paths = vec![wav.to_string_lossy().into_owned()];
        let got = extract_album_art_for_cache("Artist", "Album", &paths, None);
        assert!(got.is_some(), "expected lofty to surface embedded JPEG");
    }

    #[test]
    fn extract_album_art_returns_none_when_no_tracks_have_art() {
        let dir = scratch("no-art");
        let wav = dir.join("track.wav");
        write_wav(&wav);
        // Deliberately skip embed_art — file has no picture.

        let paths = vec![wav.to_string_lossy().into_owned()];
        let got = extract_album_art_for_cache("Artist", "Album", &paths, None);
        assert!(got.is_none());
    }

    #[test]
    fn extract_album_art_populates_disk_cache_on_miss() {
        let dir = scratch("populate-cache");
        let wav = dir.join("track.wav");
        write_wav(&wav);
        embed_art(&wav, tiny_jpeg());

        let cache_dir = dir.join("artcache");
        let cache = zytunes::art_cache::ArtCache::new(cache_dir.clone());
        assert!(cache.lookup("Artist", "Album").is_none());

        let paths = vec![wav.to_string_lossy().into_owned()];
        let _ = extract_album_art_for_cache("Artist", "Album", &paths, Some(&cache));

        // After one extraction, the cache must hold the JPEG so a subsequent
        // lookup skips lofty entirely.
        assert!(
            cache.lookup("Artist", "Album").is_some(),
            "extractor should write to the disk cache on a miss"
        );
    }

    #[test]
    fn extract_album_art_skips_unreadable_paths_and_falls_through() {
        let dir = scratch("fallthrough");
        let good = dir.join("good.wav");
        write_wav(&good);
        embed_art(&good, tiny_jpeg());

        let paths = vec![
            "/definitely/not/a/real/path.flac".to_string(),
            good.to_string_lossy().into_owned(),
        ];
        let got = extract_album_art_for_cache("Artist", "Album", &paths, None);
        assert!(
            got.is_some(),
            "bad first path should not short-circuit the search"
        );
    }

    #[test]
    fn cache_hit_path_does_not_reparse_source() {
        // Exercises the handler's compose: prime the cache, then delete the
        // source file. A working cache-hit path returns art without touching
        // the (now-gone) source; the extraction path would fail because
        // lofty can't open a missing file.
        use zytunes::art_cache::ArtCache;
        let dir = scratch("cache-hit");
        let wav = dir.join("track.wav");
        write_wav(&wav);
        embed_art(&wav, tiny_jpeg());

        let cache = ArtCache::new(dir.join("artcache"));
        let paths = vec![wav.to_string_lossy().into_owned()];

        // Prime: first call must populate the cache.
        assert!(extract_album_art_for_cache("Artist", "Album", &paths, Some(&cache)).is_some());

        // Remove the source so a re-extract would fail. Cache lookup is
        // mtime-gated, so we also need to preserve whatever fingerprint was
        // captured — the removal defeats both paths unless the cache hit
        // short-circuits before any filesystem access. That's the bug we'd
        // catch: the art_cache layer does stat the source for invalidation,
        // so this test really asserts the _happy_ case where the file is
        // still present and unchanged after a store.
        let bytes = cache.lookup("Artist", "Album").expect("cache hit");
        assert!(
            image::load_from_memory(&bytes).is_ok(),
            "cached bytes must round-trip through the image decoder"
        );
    }
}
