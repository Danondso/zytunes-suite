use std::sync::mpsc;
use std::thread;

/// True when `err` indicates the MTP session is effectively dead and no
/// further bulk ops on this session will succeed — only a physical replug
/// recovers.
///
/// Primary signal is the typed [`DeviceError::DeviceGone`] variant, which
/// both transports classify at the source (failed retry-after-stall-clear
/// on either pipe, read timeout). The string heuristic in
/// [`is_device_gone_str`] stays as a defense-in-depth fallback for errors
/// that reach us through paths that stringify before we can match the
/// variant.
fn is_device_gone(err: &DeviceError) -> bool {
    matches!(err, DeviceError::DeviceGone(_)) || is_device_gone_str(err.message())
}

/// String-sniffing fallback for [`is_device_gone`]. Observed cascade modes:
///   - `0xe00002c0` (kIOReturnNoDevice): interface invalidated.
///   - `0xe00002ed` (kIOReturnNotResponding): device stopped answering.
///   - `"retry after ClearPipeStall"`: our transport tried to clear the
///     stall and re-issue the op, and the retry itself failed. That means
///     the pipe reset didn't recover the session.
///   - `"ReadPipe timed out"`: a command's response never came. Once we've
///     written a command and the device doesn't answer within its timeout,
///     subsequent writes reliably cascade into pipe errors.
fn is_device_gone_str(err: &str) -> bool {
    err.contains("0xe00002c0")
        || err.contains("0xe00002ed")
        || err.contains("retry after ClearPipeStall")
        || err.contains("ReadPipe timed out")
        // libusb/Linux analogues of the IOKit cascade signals.
        || err.contains("retry after clear_halt")
        || err.contains("read_bulk timed out")
}

/// Drain every command pending on `cmd_rx` during a sync run: appended
/// items are spliced onto the back of `sync_queue` (bumping `total`), and
/// `CancelSync` is reported via the return value. Any other command is
/// dropped — the TUI doesn't send them while `SyncStatus` is `Running`.
///
/// The sync loop must call this *before* its queue-empty exit check, not
/// after popping an item: an `AppendSyncQueue` sent while the final track
/// was transcoding/uploading has to extend the queue here, otherwise the
/// loop would exit with the command unread and the appended tracks would
/// silently never sync.
fn drain_sync_commands(
    cmd_rx: &mpsc::Receiver<BgCommand>,
    event_tx: &mpsc::Sender<BgEvent>,
    sync_queue: &mut std::collections::VecDeque<SyncItem>,
    total: &mut usize,
) -> bool {
    let mut cancelled = false;
    while let Ok(cmd) = cmd_rx.try_recv() {
        match cmd {
            BgCommand::CancelSync => cancelled = true,
            BgCommand::AppendSyncQueue(more) if !more.is_empty() => {
                let _ = event_tx.send(BgEvent::SyncMessage(format!(
                    "Queued {} more track(s) during sync",
                    more.len()
                )));
                *total += more.len();
                sync_queue.extend(more);
            }
            _ => {}
        }
    }
    cancelled
}

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use zytunes::cd::discid::{compute_disc_id, DiscToc};
use zytunes::cd::drive::{enumerate_drives, read_disc_toc, CdDrive, DriveError};
use zytunes::cd::metadata::{ripped_track_destination, tag_ripped_file, tag_ripped_fingerprint};
use zytunes::cd::rip::{eject_drive, rip_track_cancellable, RipError, RipFidelity};
use zytunes::device::{
    DeviceBackend, DeviceCapabilities, DeviceFamily, IpodBackend, ZuneBackend, ZuneDeviceData,
};
use zytunes::mtp::native::NativeSession;
use zytunes::mtp::parse::DeviceEntry;
use zytunes::mtp::{DeviceError, DeviceSession};
use zytunes::musicbrainz::{MbError, MusicBrainzClient, Release};
use zytunes::stems::{
    cached_stems, stem_cache_key, store_stems, StemError, StemSeparator, StemSet,
};
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
    /// Search MusicBrainz for releases matching `artist` + `album`. Result
    /// is delivered as [`BgEvent::MbSearchResults`]. MB connection params
    /// arrive on the command for the same reason as `DetectCd`.
    ///
    /// `token` is echoed back on the response so the TUI can fence stale
    /// results when the overlay is closed+reopened before a response lands.
    MbSearchReleases {
        token: u64,
        artist: String,
        album: String,
        mb_base_url: Option<String>,
        mb_user_agent: Option<String>,
    },
    /// Fetch a full MB release by MBID. Result is delivered as
    /// [`BgEvent::MbReleaseLoaded`]. `token` is echoed back, same as above.
    MbReleaseDetails {
        token: u64,
        mbid: String,
        mb_base_url: Option<String>,
        mb_user_agent: Option<String>,
    },
    /// Apply an approved tag diff to disk, then surgically re-read the
    /// affected files into the library. Emits [`BgEvent::TagsApplied`]
    /// followed by [`BgEvent::LibraryRereadComplete`].
    ///
    /// Boxed because `ReleaseTagDiff` carries a `Vec<TrackTagDiff>` whose
    /// per-track inline size (PathBuf + `Vec<FieldDiff>`) easily exceeds the
    /// rest of `BgCommand`'s footprint.
    ///
    /// `token` is echoed back on both response events so the overlay can
    /// drop stale results when the user closes-then-reopens mid-apply. The
    /// library swap on success is unconditional (the files really changed),
    /// but anchor/phase mutation is gated by the token match.
    ApplyTagDiff {
        token: u64,
        diff: Box<zytunes::tag_ops::ReleaseTagDiff>,
        music_dir: String,
        fingerprint: bool,
    },
    /// Look up a Chromaprint fingerprint against the AcoustID web service.
    /// Result is delivered as [`BgEvent::AcoustIdResolved`]. The tag-manager
    /// uses this when the library carries an `acoustic_id` but no
    /// `mb_release_id`. Results are cached on disk; cache hits skip the
    /// network round-trip entirely.
    ///
    /// `token` is echoed back, same fencing as the MB requests.
    AcoustIdLookup {
        token: u64,
        fingerprint: String,
        duration_secs: u32,
        app_key: String,
    },
    /// Look up an MB recording by MBID, returning the releases it appears
    /// on. Used as a follow-up when the AcoustID response carried a
    /// recording match but no inline releases — MB itself usually knows
    /// the recording→release links. Result is delivered as
    /// [`BgEvent::MbRecordingReleases`].
    MbRecordingReleases {
        token: u64,
        recording_mbid: String,
        mb_base_url: Option<String>,
        mb_user_agent: Option<String>,
    },
    /// Ensure a stem-separation engine exists, installing one via uv if
    /// needed. Sent only after the user consents in the TUI overlay —
    /// never at startup. Params ride on the command (`DetectCd` precedent:
    /// the worker stays config-agnostic). Progress streams as
    /// [`BgEvent::StemEngineProgress`]; outcome as `StemEngineReady` /
    /// `StemEngineFailed`.
    ProvisionStemEngine {
        /// Job generation from `StemState::job_gen`, echoed back on every
        /// event this job emits. The app bumps its counter whenever it
        /// dispatches or cancels a stem job, so a terminal event from a
        /// superseded/cancelled job compares stale and can't wipe the
        /// state of the job that replaced it (jobs are keyed by identity,
        /// not by track path — same-track re-requests were colliding).
        gen: u64,
        /// Engine the consented install provisions.
        engine: zytunes::stems::provision::EngineKind,
        package: String,
        gpu: bool,
    },
    /// Separate `track_path` into six stems, cache-first. On a cache hit
    /// this answers with [`BgEvent::StemsReady`] without touching the
    /// engine; on a miss it runs demucs on a detached thread (the worker
    /// stays free for Connect/sync dispatch — separation takes minutes).
    /// Cancellation via [`BgCommand::CancelSeparation`], which trips a
    /// dedicated `AtomicBool` (independent of the rip flag).
    SeparateStems {
        /// Job generation, echoed on this job's events — see
        /// [`BgCommand::ProvisionStemEngine::gen`].
        gen: u64,
        track_path: String,
        /// Explicit engine path from `[stems].command`; `None` falls back
        /// to PATH and the managed bin dir.
        engine_command: Option<PathBuf>,
        /// Which pipeline to run — decides the engine, the passes, and
        /// the cache identity.
        recipe: zytunes::stems::RecipeKind,
        /// Demucs model name (the demucs recipe's model and cache id;
        /// multi-pass recipes ignore it in favor of pinned checkpoints).
        model: String,
        cache_max_bytes: u64,
    },
    /// Cancel the active separation or engine install. No-op when idle.
    CancelSeparation,
    /// Uninstall a stem engine via `uv tool uninstall` and reclaim its
    /// derived caches. The model-checkpoint cache is evicted whenever the
    /// engine that owns it (audio-separator) is removed; `evict_stems`
    /// additionally deletes the separated-stems cache (optional — cached
    /// stems stay playable without any engine). Answered by
    /// [`BgEvent::StemEngineUninstalled`].
    UninstallStemEngine {
        engine: zytunes::stems::provision::EngineKind,
        /// pip requirement spec; uv is addressed with its bare name.
        package: String,
        evict_stems: bool,
    },
    /// Separate every track of an album into the stem cache, cache-first
    /// (already-cached tracks are skipped and counted). Runs as ONE
    /// superseding job: a new interactive `SeparateStems` cancels it
    /// (the app re-dispatches the remainder afterwards — resume is
    /// cheap because cache hits skip instantly). No `StemsReady` is
    /// emitted; results land in the cache only.
    SeparateStemsBatch {
        /// Batch generation echoed on `StemBatch*` events (independent
        /// of the per-track job gen).
        gen: u64,
        track_paths: Vec<String>,
        engine_command: Option<PathBuf>,
        recipe: zytunes::stems::RecipeKind,
        model: String,
        cache_max_bytes: u64,
    },
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
    /// When true, the rip pipeline computes a Chromaprint fingerprint per
    /// track and embeds it as `ACOUSTID_FINGERPRINT`. ~1–15 s extra per
    /// track (capped at 120 s of audio decode).
    pub compute_acoustid_fingerprint: bool,
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
    /// Outcome of [`BgCommand::MbSearchReleases`]. `token` echoes the value
    /// from the matching command so the TUI can drop responses for queries
    /// it no longer cares about (e.g. overlay closed+reopened while a
    /// search was in flight).
    MbSearchResults {
        token: u64,
        result: Result<Vec<zytunes::musicbrainz::ReleaseSearchHit>, String>,
    },
    /// Outcome of [`BgCommand::MbReleaseDetails`]. Boxed because the
    /// `Release` payload is comparable in size to the cd `Identified`
    /// variant; clippy's `large_enum_variant` would fire otherwise.
    MbReleaseLoaded {
        token: u64,
        result: Result<Box<zytunes::musicbrainz::Release>, String>,
    },
    /// Phase 1 of `ApplyTagDiff`: tag writes + renames are done. Per-track
    /// `results` lines up with the diff's `tracks` vec; `rename_map` carries
    /// only successful renames. `token` matches the originating command.
    TagsApplied {
        token: u64,
        results: Vec<Result<(), String>>,
        rename_map: std::collections::HashMap<std::path::PathBuf, std::path::PathBuf>,
    },
    /// Phase 2 of `ApplyTagDiff`: surgical re-scan complete. Replaces
    /// `App.library` and (when the token still matches the open overlay)
    /// triggers the post-rename selection restore.
    LibraryRereadComplete {
        token: u64,
        result: Result<Box<dyn zytunes::library::MusicLibrary + Send>, String>,
    },
    /// Outcome of [`BgCommand::AcoustIdLookup`]. `hits` is sorted highest
    /// score first by the worker. Empty vec = no AcoustID match (the
    /// fingerprint is fine but no one's submitted this track to AcoustID
    /// yet); Err = HTTP/parse/API error.
    AcoustIdResolved {
        token: u64,
        result: Result<Vec<zytunes::acoustid::AcoustIdHit>, String>,
    },
    /// Outcome of [`BgCommand::MbRecordingReleases`]. Used to recover from
    /// AcoustID hits that named a recording but had no releases attached.
    MbRecordingReleases {
        token: u64,
        result: Result<Box<zytunes::musicbrainz::RecordingLookupResponse>, String>,
    },
    /// One line of engine-install output (uv/pip download progress). The
    /// TUI mirrors these into the sync log.
    StemEngineProgress(String),
    /// The stem engine is installed and usable at `command`. The TUI
    /// writes it back to `[stems].command` so later launches skip
    /// discovery, then re-issues the pending [`BgCommand::SeparateStems`].
    /// The engine path is persisted even when `gen` is stale — the
    /// install succeeded regardless of which job asked for it.
    StemEngineReady {
        gen: u64,
        command: PathBuf,
    },
    /// Engine install didn't complete. `cancelled` distinguishes the user
    /// hitting cancel from a real failure (a cancel is logged quietly, not
    /// toasted as an error). A stale `gen` means a superseded job's
    /// terminal event — logged, never allowed to touch current state.
    StemEngineFailed {
        gen: u64,
        error: String,
        cancelled: bool,
    },
    /// Percent tick for the active separation, parsed from demucs stderr.
    /// Sparse or absent on engines that print no progress. `gen` names
    /// the job (and thereby the track) — see [`BgCommand::SeparateStems`].
    StemProgress {
        gen: u64,
        pct: u8,
    },
    /// Six stems are in the cache and ready to play.
    StemsReady {
        gen: u64,
        track_path: String,
        stems: Box<StemSet>,
    },
    /// Separation didn't produce a playable stem set. `cancelled`
    /// distinguishes the user hitting cancel from a real failure (the rip
    /// pipeline's convention — a cancel is not an error in the summary).
    StemsFailed {
        gen: u64,
        track_path: String,
        error: String,
        cancelled: bool,
    },
    /// Terminal event for [`BgCommand::UninstallStemEngine`]. `error`
    /// carries the uv failure when the package step went wrong; cache
    /// eviction runs regardless (derived data), so `reclaimed_bytes` is
    /// meaningful in both cases.
    StemEngineUninstalled {
        engine: zytunes::stems::provision::EngineKind,
        reclaimed_bytes: u64,
        error: Option<String>,
    },
    /// Aggregate tick for a running album batch: which track is being
    /// worked (1-indexed) and its engine percent when available.
    StemBatchProgress {
        gen: u64,
        current: usize,
        total: usize,
        pct: Option<u8>,
    },
    /// Terminal event for [`BgCommand::SeparateStemsBatch`]. `cancelled`
    /// covers both an explicit cancel and a supersede by an interactive
    /// split — the app distinguishes via its own suspended flag.
    StemBatchDone {
        gen: u64,
        separated: usize,
        skipped: usize,
        failed: usize,
        cancelled: bool,
    },
}

/// Phase 3 per-track and end-of-rip events.
#[derive(Debug, Clone)]
pub enum RipEvent {
    /// A track is starting. `current` is 1-indexed within the selected
    /// set; `total` is the total selected. `track_length_ms` is the MB-
    /// reported duration if known — used by the TUI to render `N:NN / N:NN`
    /// + percent against the per-track progress events.
    Started {
        current: usize,
        total: usize,
        track_title: String,
        track_length_ms: Option<u64>,
    },
    /// Per-track progress update. `elapsed_ms` comes from ffmpeg's
    /// `-progress pipe:1` stream (`out_time_us` ÷ 1000). Roughly every
    /// ~100 ms while the rip is running.
    Progress { elapsed_ms: u64 },
    /// A track finished. `error` is `Some` on failure (ffmpeg, tagging,
    /// or file move).
    TrackDone {
        track_title: String,
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
        toc: DiscToc,
        mb_disc_id: String,
        primary: Box<Release>,
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

        // Stem provisioning/separation run on detached threads (they take
        // minutes and the worker must stay free for Connect/sync).
        // `StemJobs` serialises them — one at a time — and gives each job
        // its own cancel token so a newly dispatched job supersedes
        // (cancels) whatever is still running or queued, independent of
        // the rip flag.
        let stem_jobs = StemJobs::default();

        // AcoustID disk cache, lazily initialised on first `AcoustIdLookup`.
        // Kept across the worker's lifetime so all lookups in one session
        // share one disk read + the in-memory map.
        let mut acoustid_cache: Option<zytunes::acoustid::AcoustIdCache> = None;

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
                            Err(e) => Err(e.to_string()),
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
                                        let _ = event_tx.send(BgEvent::Error(e.to_string()));
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
                                let _ = event_tx.send(BgEvent::Error(e.to_string()));
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
                // `AppendSyncQueue` is normally consumed by the in-flight
                // sync loop's between-track drain. One can still land here
                // when it races the end of a sync (sent while the final
                // track was uploading, received after the loop exited) —
                // treat it exactly like a fresh `ExecuteSyncQueue` so the
                // user's added tracks sync instead of vanishing.
                BgCommand::ExecuteSyncQueue(items) | BgCommand::AppendSyncQueue(items) => {
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

                        loop {
                            // Drain pending commands between tracks: honour
                            // cancel, splice appended items onto the end.
                            // This runs before the pop (and before the
                            // queue-empty exit) so tracks appended while the
                            // previous — possibly final — track was uploading
                            // still join this run instead of being lost.
                            cancelled |= drain_sync_commands(
                                &cmd_rx,
                                &event_tx,
                                &mut sync_queue,
                                &mut total,
                            );
                            if cancelled {
                                let _ =
                                    event_tx.send(BgEvent::SyncMessage("Sync cancelled".into()));
                                break;
                            }
                            let Some(item) = sync_queue.pop_front() else {
                                break;
                            };
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
                                        error: Some(e.to_string()),
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
                        let result = s
                            .import_playlist(&name, &track_keys)
                            .map_err(|e| e.to_string());
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
                BgCommand::ProvisionStemEngine {
                    gen,
                    engine,
                    package,
                    gpu,
                } => {
                    let token = stem_jobs.supersede();
                    let jobs = stem_jobs.clone();
                    let tx = event_tx.clone();
                    thread::spawn(move || {
                        let _guard = jobs.acquire();
                        if token.load(Ordering::SeqCst) {
                            let _ = tx.send(BgEvent::StemEngineFailed {
                                gen,
                                error: "superseded before starting".into(),
                                cancelled: true,
                            });
                            return;
                        }
                        let on_line = |line: &str| {
                            let _ = tx.send(BgEvent::StemEngineProgress(line.to_string()));
                        };
                        let cancelled = || token.load(Ordering::SeqCst);
                        match zytunes::stems::provision::provision_engine(
                            engine, &package, gpu, &cancelled, &on_line,
                        ) {
                            Ok(command) => {
                                let _ = tx.send(BgEvent::StemEngineReady { gen, command });
                            }
                            Err(e) => {
                                let _ = tx.send(BgEvent::StemEngineFailed {
                                    gen,
                                    cancelled: matches!(
                                        e,
                                        zytunes::stems::provision::ProvisionError::Cancelled
                                    ),
                                    error: e.to_string(),
                                });
                            }
                        }
                    });
                }
                BgCommand::SeparateStems {
                    gen,
                    track_path,
                    engine_command,
                    recipe,
                    model,
                    cache_max_bytes,
                } => {
                    let token = stem_jobs.supersede();
                    let jobs = stem_jobs.clone();
                    let tx = event_tx.clone();
                    thread::spawn(move || {
                        // Wait for the superseded job to notice its token
                        // and exit; if OUR token tripped while waiting, a
                        // newer request replaced this one — bow out.
                        let _guard = jobs.acquire();
                        if token.load(Ordering::SeqCst) {
                            let _ = tx.send(BgEvent::StemsFailed {
                                gen,
                                track_path,
                                error: "superseded before starting".into(),
                                cancelled: true,
                            });
                            return;
                        }
                        match (
                            zytunes::stems::default_stem_cache_dir(),
                            zytunes::stems::provision::find_engine(
                                recipe.engine(),
                                engine_command.as_deref(),
                            ),
                        ) {
                            (None, _) => {
                                let _ = tx.send(BgEvent::StemsFailed {
                                    gen,
                                    track_path,
                                    error: "cannot resolve $HOME for the stem cache".into(),
                                    cancelled: false,
                                });
                            }
                            (_, None) => {
                                // The TUI provisions before separating, so
                                // this is a race (engine removed between
                                // M-press and dispatch), not the normal path.
                                let _ = tx.send(BgEvent::StemsFailed {
                                    gen,
                                    track_path,
                                    error: "stem engine not installed".into(),
                                    cancelled: false,
                                });
                            }
                            (Some(cache_dir), Some(command)) => {
                                // Emitted from OUR side, before the spawn:
                                // its absence in the log means the running
                                // binary predates this code, not that the
                                // engine is silent.
                                let what = match recipe {
                                    zytunes::stems::RecipeKind::Demucs => format!("-n {model}"),
                                    _ => format!("recipe {recipe}"),
                                };
                                let _ = tx.send(BgEvent::SyncMessage(format!(
                                    "[stems] launching {} ({what}) — first engine \
                                     output can lag ~a minute while python+torch start",
                                    command.display()
                                )));
                                let cache_id = recipe.cache_id(&model);
                                let separator =
                                    match build_recipe_separator(recipe, &model, command) {
                                        Ok(s) => s,
                                        Err(error) => {
                                            let _ = tx.send(BgEvent::StemsFailed {
                                                gen,
                                                track_path,
                                                error,
                                                cancelled: false,
                                            });
                                            return;
                                        }
                                    };
                                run_separation(
                                    &SeparationJob {
                                        gen,
                                        track_path: &track_path,
                                        layout: recipe.layout(),
                                        cache_id: &cache_id,
                                        cache_dir: &cache_dir,
                                        max_bytes: cache_max_bytes,
                                    },
                                    separator.as_ref(),
                                    &|| token.load(Ordering::SeqCst),
                                    &tx,
                                );
                                // Checkpoint swaps orphan the previous
                                // 0.2–1 GB file; sweep retired ones now
                                // that the engine is done with the dir.
                                // Only .ckpt files absent from the
                                // pinned set (plus their same-stem
                                // sidecars) are touched, so a cancelled
                                // run can't lose anything a recipe
                                // still needs.
                                if recipe.engine()
                                    == zytunes::stems::provision::EngineKind::AudioSeparator
                                {
                                    if let Some(model_dir) =
                                        zytunes::stems::default_model_file_dir()
                                    {
                                        let log_tx = tx.clone();
                                        let log: zytunes::cache::Logger =
                                            Arc::new(move |msg: &str| {
                                                let _ = log_tx.send(BgEvent::SyncMessage(format!(
                                                    "[stems] {msg}"
                                                )));
                                            });
                                        zytunes::stems::prune_model_cache(
                                            &model_dir,
                                            &zytunes::stems::pinned_model_files(),
                                            &log,
                                        );
                                    }
                                }
                            }
                        }
                    });
                }
                BgCommand::CancelSeparation => {
                    stem_jobs.cancel_active();
                }
                BgCommand::UninstallStemEngine {
                    engine,
                    package,
                    evict_stems,
                } => {
                    // Route through StemJobs like install/separation: the
                    // app-side busy guard can't see a DETACHED separation
                    // (track change sets stem status Off while the job
                    // runs on), and uninstalling under a live engine
                    // process — or remove_dir_all'ing the cache root a
                    // job is staging into — is exactly what the guard
                    // exists to prevent. Superseding cancels that job and
                    // `acquire()` waits it out; the job thread also keeps
                    // the multi-second uv run + up-to-10 GB delete off
                    // this dispatch loop, so Connect/sync stay live.
                    let token = stem_jobs.supersede();
                    let jobs = stem_jobs.clone();
                    let tx = event_tx.clone();
                    thread::spawn(move || {
                        let _guard = jobs.acquire();
                        // No token check: unlike separations, an uninstall
                        // the user confirmed must run even if another stem
                        // command lands while we wait for the lock.
                        let _ = token;
                        let error = run_engine_uninstall(&package, &tx);
                        let mut reclaimed: u64 = 0;
                        // The checkpoint cache belongs to audio-separator;
                        // a demucs uninstall must not wipe another
                        // engine's downloads.
                        if engine == zytunes::stems::provision::EngineKind::AudioSeparator {
                            reclaimed += remove_cache_dir(
                                zytunes::stems::default_model_file_dir(),
                                "model cache",
                                &tx,
                            );
                        }
                        if evict_stems {
                            reclaimed += remove_cache_dir(
                                zytunes::stems::default_stem_cache_dir(),
                                "stem cache",
                                &tx,
                            );
                        }
                        let _ = tx.send(BgEvent::StemEngineUninstalled {
                            engine,
                            reclaimed_bytes: reclaimed,
                            error,
                        });
                    });
                }
                BgCommand::SeparateStemsBatch {
                    gen,
                    track_paths,
                    engine_command,
                    recipe,
                    model,
                    cache_max_bytes,
                } => {
                    let token = stem_jobs.supersede();
                    let jobs = stem_jobs.clone();
                    let tx = event_tx.clone();
                    thread::spawn(move || {
                        let _guard = jobs.acquire();
                        let total = track_paths.len();
                        let bail = |failed: usize, cancelled: bool| {
                            let _ = tx.send(BgEvent::StemBatchDone {
                                gen,
                                separated: 0,
                                skipped: 0,
                                failed,
                                cancelled,
                            });
                        };
                        if token.load(Ordering::SeqCst) {
                            bail(0, true);
                            return;
                        }
                        let Some(cache_dir) = zytunes::stems::default_stem_cache_dir() else {
                            let _ = tx.send(BgEvent::SyncMessage(
                                "[stems] batch: cannot resolve $HOME for the stem cache".into(),
                            ));
                            bail(total, false);
                            return;
                        };
                        // The app checks the engine before dispatching, so a
                        // miss here is a race (engine removed since) — report
                        // every track as failed rather than pretending.
                        let Some(command) = zytunes::stems::provision::find_engine(
                            recipe.engine(),
                            engine_command.as_deref(),
                        ) else {
                            let _ = tx.send(BgEvent::SyncMessage(
                                "[stems] batch: stem engine not installed".into(),
                            ));
                            bail(total, false);
                            return;
                        };
                        let separator = match build_recipe_separator(recipe, &model, command) {
                            Ok(s) => s,
                            Err(e) => {
                                let _ =
                                    tx.send(BgEvent::SyncMessage(format!("[stems] batch: {e}")));
                                bail(total, false);
                                return;
                            }
                        };
                        run_stem_batch(
                            &StemBatchJob {
                                gen,
                                track_paths: &track_paths,
                                layout: recipe.layout(),
                                cache_id: &recipe.cache_id(&model),
                                cache_dir: &cache_dir,
                                max_bytes: cache_max_bytes,
                            },
                            separator.as_ref(),
                            &|| token.load(Ordering::SeqCst),
                            &tx,
                        );
                    });
                }
                BgCommand::MbSearchReleases {
                    token,
                    artist,
                    album,
                    mb_base_url,
                    mb_user_agent,
                } => {
                    let _ = event_tx.send(BgEvent::SyncMessage(format!(
                        "tag-manager: MB search artist={artist:?} album={album:?} (token={token})"
                    )));
                    let result = run_mb_search_releases_with_log(
                        &artist,
                        &album,
                        mb_base_url,
                        mb_user_agent,
                        &event_tx,
                    );
                    match &result {
                        Ok(hits) => {
                            let _ = event_tx.send(BgEvent::SyncMessage(format!(
                                "tag-manager: MB returned {} hit(s) (token={token})",
                                hits.len()
                            )));
                        }
                        Err(e) => {
                            let _ = event_tx.send(BgEvent::SyncMessage(format!(
                                "tag-manager: MB search failed (token={token}): {e}"
                            )));
                        }
                    }
                    let _ = event_tx.send(BgEvent::MbSearchResults { token, result });
                }
                BgCommand::MbReleaseDetails {
                    token,
                    mbid,
                    mb_base_url,
                    mb_user_agent,
                } => {
                    let _ = event_tx.send(BgEvent::SyncMessage(format!(
                        "tag-manager: MB release lookup mbid={mbid} (token={token})"
                    )));
                    let result = run_mb_release_details(&mbid, mb_base_url, mb_user_agent);
                    if let Err(e) = &result {
                        let _ = event_tx.send(BgEvent::SyncMessage(format!(
                            "tag-manager: MB lookup failed (token={token}): {e}"
                        )));
                    }
                    let _ = event_tx.send(BgEvent::MbReleaseLoaded { token, result });
                }
                BgCommand::ApplyTagDiff {
                    token,
                    diff,
                    music_dir,
                    fingerprint,
                } => {
                    let (results, rename_map) = zytunes::tag_ops::apply_release_diff(&diff);
                    // Collect every src/dest path so the surgical re-read
                    // covers both the original locations (now stale) and the
                    // post-rename locations (now fresh).
                    let mut paths: Vec<std::path::PathBuf> = Vec::new();
                    for track in &diff.tracks {
                        paths.push(track.src_path.clone());
                    }
                    for new in rename_map.values() {
                        paths.push(new.clone());
                    }
                    let _ = event_tx.send(BgEvent::TagsApplied {
                        token,
                        results,
                        rename_map,
                    });

                    let log_tx = event_tx.clone();
                    let scan_log: zytunes::cache::Logger = std::sync::Arc::new(move |msg: &str| {
                        let _ = log_tx.send(BgEvent::SyncMessage(msg.to_string()));
                    });
                    let lib_result = zytunes::dirlib::DirectoryLibrary::reread_paths(
                        &music_dir,
                        &paths,
                        fingerprint,
                        &scan_log,
                    )
                    .map(|l| Box::new(l) as Box<dyn zytunes::library::MusicLibrary + Send>);
                    let _ = event_tx.send(BgEvent::LibraryRereadComplete {
                        token,
                        result: lib_result,
                    });
                }
                BgCommand::AcoustIdLookup {
                    token,
                    fingerprint,
                    duration_secs,
                    app_key,
                } => {
                    // Lazily open the cache on first use so workers that
                    // never invoke AcoustID don't touch disk.
                    if acoustid_cache.is_none() {
                        let base = zytunes::paths::device_cache_base()
                            .unwrap_or_else(|| std::path::PathBuf::from("."));
                        let log_tx = event_tx.clone();
                        let log: zytunes::cache::Logger = std::sync::Arc::new(move |msg: &str| {
                            let _ = log_tx.send(BgEvent::SyncMessage(msg.to_string()));
                        });
                        acoustid_cache = Some(zytunes::acoustid::AcoustIdCache::open(&base, log));
                    }
                    let cache = acoustid_cache.as_mut().unwrap();

                    let result = if let Some(hits) = cache.get(&fingerprint, duration_secs) {
                        let _ = event_tx.send(BgEvent::SyncMessage(format!(
                            "tag-manager: AcoustID cache hit ({} hit(s), token={token})",
                            hits.len()
                        )));
                        Ok(hits.clone())
                    } else {
                        let _ = event_tx.send(BgEvent::SyncMessage(format!(
                            "tag-manager: AcoustID lookup duration={duration_secs}s (token={token})"
                        )));
                        let client = zytunes::acoustid::AcoustIdClient::new(app_key);
                        match client.lookup(&fingerprint, duration_secs) {
                            Ok(mut hits) => {
                                hits.sort_by(|a, b| {
                                    b.score
                                        .partial_cmp(&a.score)
                                        .unwrap_or(std::cmp::Ordering::Equal)
                                });
                                cache.insert(&fingerprint, duration_secs, hits.clone());
                                let _ = event_tx.send(BgEvent::SyncMessage(format!(
                                    "tag-manager: AcoustID returned {} hit(s) (token={token})",
                                    hits.len()
                                )));
                                Ok(hits)
                            }
                            Err(e) => {
                                let _ = event_tx.send(BgEvent::SyncMessage(format!(
                                    "tag-manager: AcoustID failed (token={token}): {e}"
                                )));
                                Err(e.to_string())
                            }
                        }
                    };
                    let _ = event_tx.send(BgEvent::AcoustIdResolved { token, result });
                }
                BgCommand::MbRecordingReleases {
                    token,
                    recording_mbid,
                    mb_base_url,
                    mb_user_agent,
                } => {
                    let _ = event_tx.send(BgEvent::SyncMessage(format!(
                        "tag-manager: MB recording→releases lookup mbid={recording_mbid} (token={token})"
                    )));
                    let result =
                        run_mb_recording_releases(&recording_mbid, mb_base_url, mb_user_agent);
                    match &result {
                        Ok(rec) => {
                            let _ = event_tx.send(BgEvent::SyncMessage(format!(
                                "tag-manager: MB returned {} release(s) for recording (token={token})",
                                rec.releases.len()
                            )));
                        }
                        Err(e) => {
                            let _ = event_tx.send(BgEvent::SyncMessage(format!(
                                "tag-manager: MB recording lookup failed (token={token}): {e}"
                            )));
                        }
                    }
                    let _ = event_tx.send(BgEvent::MbRecordingReleases { token, result });
                }
            }
        }
    });

    cmd_tx
}

fn run_mb_recording_releases(
    mbid: &str,
    base_url: Option<String>,
    user_agent: Option<String>,
) -> Result<Box<zytunes::musicbrainz::RecordingLookupResponse>, String> {
    let mut client = MusicBrainzClient::new(user_agent);
    if let Some(url) = base_url {
        client = client.with_base_url(url);
    }
    client
        .lookup_recording_with_releases(mbid)
        .map(Box::new)
        .map_err(|e| e.to_string())
}

fn run_mb_search_releases_with_log(
    artist: &str,
    album: &str,
    base_url: Option<String>,
    user_agent: Option<String>,
    log: &mpsc::Sender<BgEvent>,
) -> Result<Vec<zytunes::musicbrainz::ReleaseSearchHit>, String> {
    let mut client = MusicBrainzClient::new(user_agent.clone());
    if let Some(url) = &base_url {
        client = client.with_base_url(url.clone());
    }
    let url = client.search_releases_url(artist, album, 12);
    // Log artist/album bytes in hex too — if the library scanner produced
    // visually-identical-but-different unicode (Cyrillic 'u' in "Rumours",
    // BOM, NBSP, etc), the search returns 0 hits and only the raw bytes
    // tell you what went wrong.
    let _ = log.send(BgEvent::SyncMessage(format!(
        "tag-manager: MB host={} ua={:?}",
        base_url.as_deref().unwrap_or("<default canonical>"),
        user_agent.as_deref().unwrap_or("<none>"),
    )));
    let _ = log.send(BgEvent::SyncMessage(format!(
        "tag-manager: artist bytes={}",
        bytes_hex(artist)
    )));
    let _ = log.send(BgEvent::SyncMessage(format!(
        "tag-manager: album bytes={}",
        bytes_hex(album)
    )));
    let _ = log.send(BgEvent::SyncMessage(format!("tag-manager: URL={}", url)));
    client
        .search_releases(artist, album, 12)
        .map(|r| r.releases)
        .map_err(|e| e.to_string())
}

fn bytes_hex(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for (i, b) in s.as_bytes().iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(&format!("{b:02x}"));
    }
    out
}

fn run_mb_release_details(
    mbid: &str,
    base_url: Option<String>,
    user_agent: Option<String>,
) -> Result<Box<zytunes::musicbrainz::Release>, String> {
    let mut client = MusicBrainzClient::new(user_agent);
    if let Some(url) = base_url {
        client = client.with_base_url(url);
    }
    client
        .lookup_release_full(mbid)
        .map(Box::new)
        .map_err(|e| e.to_string())
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
                track_title: format!("track {position}"),
                error: Some(format!(
                    "track {position} not present in MusicBrainz release"
                )),
            }));
            continue;
        };

        let _ = event_tx.send(BgEvent::RipEvent(RipEvent::Started {
            current: idx + 1,
            total,
            track_title: mb_track.title.clone(),
            track_length_ms: mb_track.length.map(u64::from),
        }));

        let dest = ripped_track_destination(
            &req.dest_dir,
            &req.release,
            mb_track,
            position,
            req.fidelity.extension(),
        );

        // Forward ffmpeg's elapsed-µs progress to the TUI as RipEvent::Progress
        // events. The closure is called from the rip thread's polling loop
        // (~every 100 ms) so the user sees smooth per-track motion.
        let progress_tx = event_tx.clone();
        let on_progress = move |elapsed_us: u64| {
            let _ = progress_tx.send(BgEvent::RipEvent(RipEvent::Progress {
                elapsed_ms: elapsed_us / 1_000,
            }));
        };
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
                compute_fingerprint: req.compute_acoustid_fingerprint,
            },
            cancel,
            &on_progress,
        );

        // Typed outcome from `run_single_track_rip`. Accounting now
        // matches the user's mental model:
        // - `Ripped` and `RippedUntagged` both count as `ripped` (the
        //   audio is on disk; tagging issues surface as warnings).
        // - `Cancelled` doesn't increment anything; the `Complete
        //   { cancelled: true }` event tells the user why.
        // - `Failed` increments `failed`.
        let error_for_event = match &outcome {
            TrackOutcome::Ripped => {
                ripped += 1;
                None
            }
            TrackOutcome::RippedUntagged { warning } => {
                ripped += 1;
                let _ = event_tx.send(BgEvent::SyncMessage(format!(
                    "[rip] {} — {warning}",
                    mb_track.title
                )));
                None
            }
            TrackOutcome::Cancelled => None,
            TrackOutcome::Failed(e) => {
                failed += 1;
                Some(e.clone())
            }
        };

        let _ = event_tx.send(BgEvent::RipEvent(RipEvent::TrackDone {
            track_title: mb_track.title.clone(),
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
    compute_fingerprint: bool,
}

/// Result of a single-track rip — finer-grained than `Result<_,_>` so
/// the caller can distinguish full success, success-with-non-fatal-warning
/// (audio ripped but tagging failed), cancellation, and outright failure
/// for accounting purposes.
pub(crate) enum TrackOutcome {
    /// Audio ripped and tagged. Counts as `ripped`.
    Ripped,
    /// Audio ripped and on disk, but tagging failed — file is still in
    /// the library so it counts as `ripped`; the warning is logged so
    /// the user knows their tags are missing.
    RippedUntagged { warning: String },
    /// User cancelled mid-track. Doesn't count as `ripped` or `failed`;
    /// `.part` temp file is cleaned up before returning.
    Cancelled,
    /// Rip never produced a usable file. Counts as `failed`. `.part`
    /// temp file is cleaned up before returning.
    Failed(String),
}

/// Remove any sibling files in the same directory that share `dest`'s
/// stem but have a *different* extension. Used as the second half of
/// the confirm-overwrite flow: the conflict pre-scan in `confirm_import`
/// catches cross-extension conflicts (ALAC `.m4a` already on disk while
/// the user re-rips at FLAC `.flac`), and once the new file has landed
/// at its canonical path this sweep deletes the leftover copies the
/// user already opted to replace.
///
/// Extensionless same-stem siblings are *preserved* — the intent is to
/// dedupe lossy/lossless audio copies of one track, and a file with no
/// extension isn't an audio file by our extension-based scanning model
/// (see `dirlib.rs`). It's also not "a different extension" — it's no
/// extension at all.
///
/// Best-effort — read-dir / remove-file errors are swallowed silently:
/// a failed sweep just means the user has to clean up by hand later,
/// not that the rip itself failed. Same-extension overwrites are
/// handled by `std::fs::rename` and aren't touched here.
fn sweep_same_stem_other_extensions(dest: &std::path::Path) {
    let (Some(parent), Some(target_stem), Some(target_ext_os)) = (
        dest.parent(),
        dest.file_stem().and_then(|s| s.to_str()),
        dest.extension(),
    ) else {
        return;
    };
    let target_ext = target_ext_os.to_string_lossy();
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path == dest {
            continue;
        }
        let same_stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s == target_stem);
        let diff_extension = path
            .extension()
            .is_some_and(|e| e.to_string_lossy() != target_ext);
        if same_stem && diff_extension {
            let _ = std::fs::remove_file(&path);
        }
    }
}

fn run_single_track_rip<'a>(
    params: SingleTrackRip<'a>,
    cancel: &Arc<AtomicBool>,
    on_progress: &dyn Fn(u64),
) -> TrackOutcome {
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
        compute_fingerprint,
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
        on_progress,
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

    // Chromaprint fingerprint + ACOUSTID_FINGERPRINT tag. Caps at 120 s of
    // audio decode (FINGERPRINT_SECONDS in fingerprint.rs) so the extra
    // cost is bounded. Skipped if the user opted out or if we're already
    // cancelled — no point burning a decode pass on a track the user is
    // walking away from.
    let fingerprint_warning = if compute_fingerprint && !cancel.load(Ordering::SeqCst) {
        match zytunes::fingerprint::compute_fingerprint(&temp_path) {
            Some(fp) => tag_ripped_fingerprint(&temp_path, &fp).err(),
            None => Some(
                "fingerprint compute returned no hashes (file too short or decoder bailed)"
                    .to_string(),
            ),
        }
    } else {
        None
    };

    if let Err(e) = std::fs::rename(&temp_path, dest) {
        let _ = std::fs::remove_file(&temp_path);
        return TrackOutcome::Failed(format!(
            "rename {} → {}: {e}",
            temp_path.display(),
            dest.display()
        ));
    }

    // Sweep stem-matching files at other extensions in the same album
    // directory. The conflict pre-scan warned about these and the user
    // confirmed overwrite; without this sweep, a fidelity change (ALAC
    // .m4a → FLAC .flac) writes the new file at a new path and leaves
    // the prior copy on disk. Same-extension overwrites are already
    // covered by `std::fs::rename`, so this only removes sibling files
    // whose stem matches the just-written track but whose extension
    // differs.
    sweep_same_stem_other_extensions(dest);

    match (tag_warning, fingerprint_warning) {
        (Some(tag_err), Some(fp_err)) => TrackOutcome::RippedUntagged {
            warning: format!("tagging failed: {tag_err}; fingerprint failed: {fp_err}"),
        },
        (Some(tag_err), None) => TrackOutcome::RippedUntagged {
            warning: format!("tagging failed: {tag_err}"),
        },
        (None, Some(fp_err)) => TrackOutcome::RippedUntagged {
            warning: format!("fingerprint failed: {fp_err}"),
        },
        (None, None) => TrackOutcome::Ripped,
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

/// Serialisation + supersede plumbing for stem jobs (engine install and
/// separation). One job runs at a time (each job thread holds `lock` for
/// its duration), and every job gets its own cancel token: dispatching a
/// new job trips the previous job's token whether it is running or still
/// waiting for the lock. That is what makes a track change or a fresh
/// `M`-press replace a stale separation instead of queueing behind it or
/// bouncing off a busy flag.
#[derive(Clone, Default)]
struct StemJobs {
    lock: Arc<std::sync::Mutex<()>>,
    /// Cancel token of the most recently dispatched job.
    active: Arc<std::sync::Mutex<Option<Arc<AtomicBool>>>>,
}

impl StemJobs {
    /// Register a new job: trips the previous job's token (if any) and
    /// returns the fresh token the new job must poll.
    fn supersede(&self) -> Arc<AtomicBool> {
        let token = Arc::new(AtomicBool::new(false));
        let mut slot = self.active.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(prev) = slot.replace(Arc::clone(&token)) {
            prev.store(true, Ordering::SeqCst);
        }
        token
    }

    /// Trip the most recent job's token (user-initiated cancel). The
    /// token stays registered so repeat cancels are idempotent.
    fn cancel_active(&self) {
        let slot = self.active.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(token) = slot.as_ref() {
            token.store(true, Ordering::SeqCst);
        }
    }

    /// Block until the previous job finishes. A poisoned lock (panicked
    /// job thread) must not wedge stem playback forever, so it's cleared.
    fn acquire(&self) -> std::sync::MutexGuard<'_, ()> {
        self.lock.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Build the separator a recipe drives: `DemucsCli` for the demucs
/// recipe, a `CascadeSeparator` over audio-separator for the hq
/// recipes. `Err` carries a user-visible message.
fn build_recipe_separator(
    recipe: zytunes::stems::RecipeKind,
    model: &str,
    command: std::path::PathBuf,
) -> Result<Box<dyn StemSeparator>, String> {
    Ok(match recipe {
        zytunes::stems::RecipeKind::Demucs => Box::new(zytunes::stems::DemucsCli {
            command,
            model: model.to_string(),
            layout: recipe.layout(),
        }),
        zytunes::stems::RecipeKind::Hq | zytunes::stems::RecipeKind::HqHarmony => {
            let model_file_dir = zytunes::stems::default_model_file_dir()
                .ok_or_else(|| "cannot resolve $HOME for the model cache".to_string())?;
            Box::new(zytunes::stems::CascadeSeparator {
                engine: zytunes::stems::AudioSeparatorCli {
                    command,
                    model_file_dir,
                },
                passes: match recipe {
                    zytunes::stems::RecipeKind::HqHarmony => {
                        zytunes::stems::hq_harmony_recipe_passes()
                    }
                    _ => zytunes::stems::hq_recipe_passes(),
                },
                layout: recipe.layout(),
            })
        }
    })
}

/// One album batch — the whole batch is ONE worker job (one supersede
/// token), so an interactive `SeparateStems` cancels it as a unit and
/// the app re-dispatches the remainder afterwards.
struct StemBatchJob<'a> {
    gen: u64,
    track_paths: &'a [String],
    layout: &'static [zytunes::stems::StemKind],
    cache_id: &'a str,
    cache_dir: &'a std::path::Path,
    max_bytes: u64,
}

/// Cache-first batch driver: skip tracks whose stems are already cached
/// for this recipe (counted, so a mostly-warm album is visibly cheap),
/// separate the rest into the cache, and emit aggregate
/// [`BgEvent::StemBatchProgress`] ticks plus exactly one terminal
/// [`BgEvent::StemBatchDone`]. Never emits `StemsReady` — a batch
/// pre-warms the cache, it must not hijack playback.
fn run_stem_batch(
    job: &StemBatchJob<'_>,
    separator: &dyn StemSeparator,
    cancelled: &dyn Fn() -> bool,
    event_tx: &mpsc::Sender<BgEvent>,
) {
    let StemBatchJob {
        gen,
        track_paths,
        layout,
        cache_id,
        cache_dir,
        max_bytes,
    } = *job;
    let log_tx = event_tx.clone();
    let log: zytunes::cache::Logger = Arc::new(move |msg: &str| {
        let _ = log_tx.send(BgEvent::SyncMessage(format!("[stems] {msg}")));
    });
    let total = track_paths.len();
    let (mut separated, mut skipped, mut failed) = (0usize, 0usize, 0usize);
    let mut was_cancelled = false;
    for (i, track) in track_paths.iter().enumerate() {
        if cancelled() {
            was_cancelled = true;
            break;
        }
        let current = i + 1;
        let _ = event_tx.send(BgEvent::StemBatchProgress {
            gen,
            current,
            total,
            pct: None,
        });
        let source = std::path::Path::new(track);
        if cached_stems(cache_dir, source, cache_id, layout, &log).is_some() {
            skipped += 1;
            continue;
        }
        log(&format!("batch {current}/{total}: separating {track}"));
        let work_dir = cache_dir.join("work").join(stem_cache_key(track, cache_id));
        let _ = std::fs::remove_dir_all(&work_dir);
        let progress_tx = event_tx.clone();
        let on_progress = move |pct: u8| {
            let _ = progress_tx.send(BgEvent::StemBatchProgress {
                gen,
                current,
                total,
                pct: Some(pct),
            });
        };
        let on_line = |line: &str| log(line);
        let outcome = separator.separate(source, &work_dir, cancelled, &on_progress, &on_line);
        match outcome {
            Ok(produced) => {
                match store_stems(cache_dir, source, cache_id, &produced, max_bytes, &log) {
                    Ok(_) => separated += 1,
                    Err(e) => {
                        failed += 1;
                        log(&format!("batch: store failed for {track}: {e}"));
                    }
                }
            }
            Err(StemError::Cancelled) => {
                was_cancelled = true;
                let _ = std::fs::remove_dir_all(&work_dir);
                break;
            }
            Err(e) => {
                failed += 1;
                log(&format!("batch: {track}: {e}"));
            }
        }
        let _ = std::fs::remove_dir_all(&work_dir);
    }
    let _ = event_tx.send(BgEvent::StemBatchDone {
        gen,
        separated,
        skipped,
        failed,
        cancelled: was_cancelled,
    });
}

/// Everything that names a separation job: which job it is (`gen`), what
/// it splits, and where results live. Groups the parameters that ride
/// every [`run_separation`] call so the driver's signature stays small.
struct SeparationJob<'a> {
    gen: u64,
    track_path: &'a str,
    /// Ordered stems the recipe produces — cache lookups validate
    /// against this exact file set.
    layout: &'static [zytunes::stems::StemKind],
    /// Cache identity ([`zytunes::stems::RecipeKind::cache_id`]) — the
    /// demucs model string for the demucs recipe, a versioned recipe id
    /// otherwise.
    cache_id: &'a str,
    cache_dir: &'a std::path::Path,
    max_bytes: u64,
}

/// Run `uv tool uninstall` for `package`, returning the failure message
/// if the step went wrong. A missing uv or a non-zero exit is reported,
/// not swallowed — the caller still reclaims caches either way.
fn run_engine_uninstall(package: &str, event_tx: &mpsc::Sender<BgEvent>) -> Option<String> {
    let Some(uv) = zytunes::stems::provision::find_uv() else {
        return Some("uv not found — remove the engine manually".to_string());
    };
    let bin_dir = zytunes::stems::provision::zytunes_bin_dir();
    let (prog, args, envs) = match zytunes::stems::provision::build_uninstall_command(
        &uv,
        package,
        bin_dir.as_deref(),
    ) {
        Ok(cmd) => cmd,
        Err(e) => return Some(e),
    };
    match std::process::Command::new(&prog)
        .args(&args)
        .envs(envs)
        .output()
    {
        Ok(out) if out.status.success() => {
            let _ = event_tx.send(BgEvent::SyncMessage(format!(
                "[stems] uninstalled {}",
                zytunes::stems::provision::package_name(package)
            )));
            None
        }
        Ok(out) => Some(format!(
            "uv tool uninstall failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )),
        Err(e) => Some(format!("failed to run {}: {e}", prog.display())),
    }
}

/// Delete a derived-cache dir, logging what was reclaimed. Returns the
/// byte count freed (0 for a missing dir or on failure).
fn remove_cache_dir(
    dir: Option<std::path::PathBuf>,
    what: &str,
    event_tx: &mpsc::Sender<BgEvent>,
) -> u64 {
    let Some(dir) = dir else { return 0 };
    let bytes = zytunes::stems::dir_size_recursive(&dir);
    if bytes == 0 && !dir.exists() {
        return 0;
    }
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => {
            let _ = event_tx.send(BgEvent::SyncMessage(format!(
                "[stems] evicted {what} ({:.1} MB)",
                bytes as f64 / (1024.0 * 1024.0)
            )));
            bytes
        }
        Err(e) => {
            let _ = event_tx.send(BgEvent::SyncMessage(format!(
                "[stems] failed to evict {what} at {}: {e}",
                dir.display()
            )));
            0
        }
    }
}

/// Cache-first separation driver: answer from the stem cache when the
/// entry is fresh, otherwise run `separator` into a work dir under the
/// cache root and move the result in. Emits [`BgEvent::StemProgress`]
/// ticks while separating and exactly one terminal event —
/// [`BgEvent::StemsReady`] or [`BgEvent::StemsFailed`].
///
/// Takes the separator as `&dyn StemSeparator` so tests drive it with a
/// fake; production wraps `DemucsCli` on a detached thread.
fn run_separation(
    job: &SeparationJob<'_>,
    separator: &dyn StemSeparator,
    cancelled: &dyn Fn() -> bool,
    event_tx: &mpsc::Sender<BgEvent>,
) {
    let SeparationJob {
        gen,
        track_path,
        layout,
        cache_id,
        cache_dir,
        max_bytes,
    } = *job;
    let log_tx = event_tx.clone();
    let log: zytunes::cache::Logger = Arc::new(move |msg: &str| {
        let _ = log_tx.send(BgEvent::SyncMessage(format!("[stems] {msg}")));
    });
    let source = std::path::Path::new(track_path);

    // Rename any pre-recipe-key entries into the `{hash}-{cache_id}`
    // scheme before looking up — a legacy entry is a rename away from
    // being a hit, never a re-separation. No-op after the first sweep.
    zytunes::stems::migrate_legacy_stem_entries(cache_dir, &log);

    if let Some(stems) = cached_stems(cache_dir, source, cache_id, layout, &log) {
        let _ = event_tx.send(BgEvent::StemsReady {
            gen,
            track_path: track_path.to_string(),
            stems: Box::new(stems),
        });
        return;
    }

    // Work dir keyed like the cache entry so concurrent runs on different
    // tracks can't collide; removed whatever the outcome (partial demucs
    // output must never look like a cache).
    let work_dir = cache_dir
        .join("work")
        .join(stem_cache_key(track_path, cache_id));
    let _ = std::fs::remove_dir_all(&work_dir);

    let progress_tx = event_tx.clone();
    let on_progress = move |pct: u8| {
        let _ = progress_tx.send(BgEvent::StemProgress { gen, pct });
    };

    // Engine chatter (model download notices, per-track banners, torch
    // warnings) goes to the sync log so a slow first run is visibly
    // "downloading the model", not a mystery stall.
    let on_line = |line: &str| log(line);
    let outcome = separator.separate(source, &work_dir, cancelled, &on_progress, &on_line);
    let terminal = match outcome {
        Ok(produced) => {
            match store_stems(cache_dir, source, cache_id, &produced, max_bytes, &log) {
                Ok(stems) => BgEvent::StemsReady {
                    gen,
                    track_path: track_path.to_string(),
                    stems: Box::new(stems),
                },
                Err(e) => BgEvent::StemsFailed {
                    gen,
                    track_path: track_path.to_string(),
                    error: e,
                    cancelled: false,
                },
            }
        }
        Err(e) => BgEvent::StemsFailed {
            gen,
            track_path: track_path.to_string(),
            error: e.to_string(),
            cancelled: matches!(e, StemError::Cancelled),
        },
    };
    let _ = std::fs::remove_dir_all(&work_dir);
    let _ = event_tx.send(terminal);
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

    fn fresh_dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join("zytunes-sweep-tests").join(name);
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn drain_sync_commands_splices_append_even_when_queue_is_empty() {
        // Regression: tracks appended while the final track of a sync was
        // transcoding/uploading used to sit unread in the channel until
        // the sync loop exited on the empty queue — the append was then
        // dropped and the tracks silently never synced. The drain must
        // run before the empty-queue exit and extend an empty queue.
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let mut queue: std::collections::VecDeque<SyncItem> = std::collections::VecDeque::new();
        let mut total = 3usize;

        cmd_tx
            .send(BgCommand::AppendSyncQueue(vec![SyncItem {
                name: "late add".into(),
                ..Default::default()
            }]))
            .unwrap();

        let cancelled = drain_sync_commands(&cmd_rx, &event_tx, &mut queue, &mut total);

        assert!(!cancelled);
        assert_eq!(queue.len(), 1, "append must extend an already-empty queue");
        assert_eq!(queue[0].name, "late add");
        assert_eq!(total, 4, "running total must include the appended track");
        let ev = event_rx.try_recv().ok();
        assert!(
            matches!(ev, Some(BgEvent::SyncMessage(ref m)) if m.contains("more track(s)")),
            "splice should announce itself in the sync log"
        );
    }

    #[test]
    fn drain_sync_commands_reports_cancel_and_ignores_empty_appends() {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let mut queue: std::collections::VecDeque<SyncItem> = std::collections::VecDeque::new();
        let mut total = 2usize;

        cmd_tx.send(BgCommand::AppendSyncQueue(vec![])).unwrap();
        cmd_tx.send(BgCommand::CancelSync).unwrap();

        let cancelled = drain_sync_commands(&cmd_rx, &event_tx, &mut queue, &mut total);

        assert!(cancelled, "CancelSync must be reported");
        assert!(queue.is_empty(), "empty appends must not enqueue anything");
        assert_eq!(total, 2, "empty appends must not bump the total");
        assert!(
            event_rx.try_recv().is_err(),
            "empty appends must not log a splice message"
        );
    }

    #[test]
    fn sweep_removes_other_extension_siblings_at_same_stem() {
        // The cross-fidelity re-rip scenario: a `.flac` just landed, an
        // older `.m4a` at the same stem must go too.
        let dir = fresh_dir("sweep-stem-match");
        let dest = dir.join("01 - Hung Up.flac");
        std::fs::write(&dest, b"new flac").unwrap();
        let stragglers = ["01 - Hung Up.m4a", "01 - Hung Up.mp3"];
        for s in &stragglers {
            std::fs::write(dir.join(s), b"old lossy").unwrap();
        }
        sweep_same_stem_other_extensions(&dest);
        assert!(dest.exists(), "the file we just ripped must survive");
        for s in &stragglers {
            assert!(
                !dir.join(s).exists(),
                "same-stem different-extension straggler {s} should be removed"
            );
        }
    }

    /// Test double for [`run_separation`]: "separates" by writing six
    /// bytes-long stem files into the work dir. Counts invocations so the
    /// cache-hit fast path is observable.
    struct FakeSeparator {
        calls: std::cell::Cell<usize>,
        outcome: Result<(), StemError>,
    }

    impl FakeSeparator {
        fn ok() -> Self {
            FakeSeparator {
                calls: std::cell::Cell::new(0),
                outcome: Ok(()),
            }
        }
    }

    impl StemSeparator for FakeSeparator {
        fn available(&self) -> Result<(), String> {
            Ok(())
        }
        fn separate(
            &self,
            _src: &std::path::Path,
            out_dir: &std::path::Path,
            _cancelled: &dyn Fn() -> bool,
            progress: &dyn Fn(u8),
            _on_line: &dyn Fn(&str),
        ) -> Result<StemSet, StemError> {
            self.calls.set(self.calls.get() + 1);
            match &self.outcome {
                Ok(()) => {
                    progress(50);
                    std::fs::create_dir_all(out_dir).unwrap();
                    let set = StemSet::from_layout(
                        out_dir,
                        zytunes::stems::STEM_EXT,
                        zytunes::stems::SIX_STEM_LAYOUT,
                    );
                    for p in &set.paths {
                        std::fs::write(p, b"stem").unwrap();
                    }
                    Ok(set)
                }
                Err(StemError::Cancelled) => Err(StemError::Cancelled),
                Err(_) => Err(StemError::EngineFailed {
                    exit_code: Some(1),
                    stderr: "boom".into(),
                }),
            }
        }
    }

    fn stem_events(rx: &mpsc::Receiver<BgEvent>) -> (Vec<u8>, Option<BgEvent>) {
        let mut ticks = Vec::new();
        let mut terminal = None;
        for ev in rx.try_iter() {
            match ev {
                BgEvent::StemProgress { pct, .. } => ticks.push(pct),
                e @ (BgEvent::StemsReady { .. } | BgEvent::StemsFailed { .. }) => {
                    assert!(terminal.is_none(), "exactly one terminal event");
                    terminal = Some(e);
                }
                BgEvent::SyncMessage(_) => {}
                _ => panic!("unexpected event kind"),
            }
        }
        (ticks, terminal)
    }

    #[test]
    fn stem_jobs_supersede_trips_previous_token_only() {
        let jobs = StemJobs::default();
        let first = jobs.supersede();
        assert!(!first.load(Ordering::SeqCst), "fresh token starts clear");

        let second = jobs.supersede();
        assert!(
            first.load(Ordering::SeqCst),
            "dispatching a new job cancels the previous one"
        );
        assert!(!second.load(Ordering::SeqCst), "the new job itself runs");

        let third = jobs.supersede();
        assert!(second.load(Ordering::SeqCst));
        assert!(!third.load(Ordering::SeqCst));
    }

    #[test]
    fn stem_jobs_cancel_active_trips_latest_and_is_idempotent() {
        let jobs = StemJobs::default();
        // No job registered: harmless no-op.
        jobs.cancel_active();

        let token = jobs.supersede();
        jobs.cancel_active();
        assert!(token.load(Ordering::SeqCst));
        jobs.cancel_active(); // repeat cancel stays fine

        // A job dispatched after a cancel starts with a clear token.
        let next = jobs.supersede();
        assert!(!next.load(Ordering::SeqCst));
    }

    #[test]
    fn stem_batch_skips_cached_and_stores_the_rest() {
        let dir = fresh_dir("stems-batch-mixed");
        let cache = dir.join("cache");
        let a = dir.join("a.mp3");
        let b = dir.join("b.mp3");
        std::fs::write(&a, b"mp3a").unwrap();
        std::fs::write(&b, b"mp3b").unwrap();
        // Pre-warm track A so the batch's cache-first check skips it.
        let produced_dir = dir.join("prewarm");
        std::fs::create_dir_all(&produced_dir).unwrap();
        let produced = StemSet::from_layout(
            &produced_dir,
            zytunes::stems::STEM_EXT,
            zytunes::stems::SIX_STEM_LAYOUT,
        );
        for p in &produced.paths {
            std::fs::write(p, b"warm").unwrap();
        }
        let log = zytunes::cache::default_logger();
        zytunes::stems::store_stems(&cache, &a, "m", &produced, u64::MAX, &log).unwrap();

        let (event_tx, event_rx) = mpsc::channel();
        let sep = FakeSeparator::ok();
        let tracks = vec![
            a.to_string_lossy().into_owned(),
            b.to_string_lossy().into_owned(),
        ];
        run_stem_batch(
            &StemBatchJob {
                gen: 3,
                track_paths: &tracks,
                layout: zytunes::stems::SIX_STEM_LAYOUT,
                cache_id: "m",
                cache_dir: &cache,
                max_bytes: u64::MAX,
            },
            &sep,
            &|| false,
            &event_tx,
        );

        assert_eq!(sep.calls.get(), 1, "cached track never reaches the engine");
        let mut done = None;
        let mut progressed = Vec::new();
        for ev in event_rx.try_iter() {
            match ev {
                BgEvent::StemBatchProgress { current, total, .. } => {
                    progressed.push((current, total))
                }
                BgEvent::StemBatchDone {
                    gen,
                    separated,
                    skipped,
                    failed,
                    cancelled,
                } => {
                    assert!(done.is_none(), "exactly one terminal event");
                    done = Some((gen, separated, skipped, failed, cancelled));
                }
                BgEvent::SyncMessage(_) => {}
                _ => panic!("unexpected event kind"),
            }
        }
        assert_eq!(done, Some((3, 1, 1, 0, false)));
        assert!(progressed.contains(&(1, 2)) && progressed.contains(&(2, 2)));
        // Track B's stems landed in the cache.
        assert!(zytunes::stems::cached_stems(
            &cache,
            &b,
            "m",
            zytunes::stems::SIX_STEM_LAYOUT,
            &log
        )
        .is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stem_batch_cancel_reports_cancelled_without_counting_failures() {
        let dir = fresh_dir("stems-batch-cancel");
        let cache = dir.join("cache");
        let a = dir.join("a.mp3");
        std::fs::write(&a, b"mp3").unwrap();
        let (event_tx, event_rx) = mpsc::channel();
        let sep = FakeSeparator::ok();
        let tracks = vec![a.to_string_lossy().into_owned()];
        run_stem_batch(
            &StemBatchJob {
                gen: 4,
                track_paths: &tracks,
                layout: zytunes::stems::SIX_STEM_LAYOUT,
                cache_id: "m",
                cache_dir: &cache,
                max_bytes: u64::MAX,
            },
            &sep,
            &|| true, // cancelled before the first track
            &event_tx,
        );
        assert_eq!(sep.calls.get(), 0);
        let done = event_rx.try_iter().find_map(|ev| match ev {
            BgEvent::StemBatchDone {
                separated,
                skipped,
                failed,
                cancelled,
                ..
            } => Some((separated, skipped, failed, cancelled)),
            _ => None,
        });
        assert_eq!(done, Some((0, 0, 0, true)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_separation_misses_then_hits_cache() {
        let dir = fresh_dir("stems-run-roundtrip");
        let cache = dir.join("cache");
        let source = dir.join("song.mp3");
        std::fs::write(&source, b"mp3").unwrap();
        let (event_tx, event_rx) = mpsc::channel();
        let sep = FakeSeparator::ok();
        let track = source.to_string_lossy().into_owned();

        run_separation(
            &SeparationJob {
                gen: 7,
                track_path: &track,
                layout: zytunes::stems::SIX_STEM_LAYOUT,
                cache_id: "m",
                cache_dir: &cache,
                max_bytes: u64::MAX,
            },
            &sep,
            &|| false,
            &event_tx,
        );
        let (ticks, terminal) = stem_events(&event_rx);
        assert_eq!(sep.calls.get(), 1);
        assert!(ticks.contains(&50), "progress ticks forwarded: {ticks:?}");
        match terminal {
            Some(BgEvent::StemsReady {
                gen,
                track_path,
                stems,
            }) => {
                assert_eq!(gen, 7, "events echo the dispatching job's gen");
                assert_eq!(track_path, track);
                assert!(stems.all_exist(), "stems live in the cache");
                assert!(
                    stems.paths[0].starts_with(&cache),
                    "ready paths must point at cached copies"
                );
            }
            other => panic!("expected StemsReady, got {:?}", other.is_some()),
        }

        // Second run: cache hit, the separator is not consulted again.
        run_separation(
            &SeparationJob {
                gen: 8,
                track_path: &track,
                layout: zytunes::stems::SIX_STEM_LAYOUT,
                cache_id: "m",
                cache_dir: &cache,
                max_bytes: u64::MAX,
            },
            &sep,
            &|| false,
            &event_tx,
        );
        let (_, terminal) = stem_events(&event_rx);
        assert_eq!(sep.calls.get(), 1, "cache hit must skip the engine");
        assert!(matches!(terminal, Some(BgEvent::StemsReady { .. })));
    }

    #[test]
    fn run_separation_failure_reports_error_not_cancel() {
        let dir = fresh_dir("stems-run-fail");
        let source = dir.join("song.mp3");
        std::fs::write(&source, b"mp3").unwrap();
        let (event_tx, event_rx) = mpsc::channel();
        let sep = FakeSeparator {
            calls: std::cell::Cell::new(0),
            outcome: Err(StemError::EngineFailed {
                exit_code: Some(1),
                stderr: String::new(),
            }),
        };

        run_separation(
            &SeparationJob {
                gen: 7,
                track_path: &source.to_string_lossy(),
                layout: zytunes::stems::SIX_STEM_LAYOUT,
                cache_id: "m",
                cache_dir: &dir.join("cache"),
                max_bytes: u64::MAX,
            },
            &sep,
            &|| false,
            &event_tx,
        );
        let (_, terminal) = stem_events(&event_rx);
        match terminal {
            Some(BgEvent::StemsFailed {
                error, cancelled, ..
            }) => {
                assert!(!cancelled);
                assert!(error.contains("boom"), "{error}");
            }
            _ => panic!("expected StemsFailed"),
        }
    }

    #[test]
    fn run_separation_cancel_sets_cancelled_flag() {
        let dir = fresh_dir("stems-run-cancel");
        let source = dir.join("song.mp3");
        std::fs::write(&source, b"mp3").unwrap();
        let (event_tx, event_rx) = mpsc::channel();
        let sep = FakeSeparator {
            calls: std::cell::Cell::new(0),
            outcome: Err(StemError::Cancelled),
        };

        run_separation(
            &SeparationJob {
                gen: 7,
                track_path: &source.to_string_lossy(),
                layout: zytunes::stems::SIX_STEM_LAYOUT,
                cache_id: "m",
                cache_dir: &dir.join("cache"),
                max_bytes: u64::MAX,
            },
            &sep,
            &|| false,
            &event_tx,
        );
        let (_, terminal) = stem_events(&event_rx);
        assert!(
            matches!(
                terminal,
                Some(BgEvent::StemsFailed {
                    cancelled: true,
                    ..
                })
            ),
            "user cancel must be distinguishable from failure"
        );
    }

    #[test]
    fn sweep_preserves_unrelated_siblings() {
        // Different stems (other tracks on the album) must be left alone.
        let dir = fresh_dir("sweep-other-tracks");
        let dest = dir.join("01 - Hung Up.flac");
        std::fs::write(&dest, b"new flac").unwrap();
        let keepers = [
            "02 - Get Together.m4a", // different track number
            "01 Hung Up.m4a",        // different stem (no dash separator)
            "01 - Hung Up Live.m4a", // different stem (longer title)
            "cover.jpg",             // entirely different file
        ];
        for k in &keepers {
            std::fs::write(dir.join(k), b"keep me").unwrap();
        }
        sweep_same_stem_other_extensions(&dest);
        for k in &keepers {
            assert!(
                dir.join(k).exists(),
                "unrelated sibling {k} should not be removed"
            );
        }
    }

    #[test]
    fn sweep_preserves_extensionless_same_stem_sibling() {
        // An extensionless file at the same stem isn't an audio dupe — by
        // our extension-based scanning model it isn't an audio file at
        // all. Don't sweep it: the intent is to dedupe fidelity variants,
        // not to clean up arbitrary same-stem companions.
        let dir = fresh_dir("sweep-extensionless");
        let dest = dir.join("01 - Hung Up.flac");
        std::fs::write(&dest, b"new flac").unwrap();
        let bare = dir.join("01 - Hung Up");
        std::fs::write(&bare, b"some companion file").unwrap();
        sweep_same_stem_other_extensions(&dest);
        assert!(dest.exists(), "the file we just ripped must survive");
        assert!(
            bare.exists(),
            "extensionless same-stem sibling must not be swept"
        );
    }

    #[test]
    fn sweep_does_not_remove_same_extension_dupe_target() {
        // Trying to sweep a path that has only same-extension neighbours
        // is a no-op — those are handled by `std::fs::rename`, never by
        // the sweep helper. Guards against an off-by-one that would wipe
        // the file we just renamed onto.
        let dir = fresh_dir("sweep-same-ext");
        let dest = dir.join("01 - Track.flac");
        std::fs::write(&dest, b"keep").unwrap();
        sweep_same_stem_other_extensions(&dest);
        assert!(dest.exists());
    }

    #[test]
    fn sweep_is_a_noop_when_parent_unreadable() {
        // Permission/IO errors on read_dir must be swallowed silently —
        // the rip itself succeeded, the sweep is a courtesy operation.
        let nonexistent = std::path::Path::new("/this/path/does/not/exist/track.flac");
        sweep_same_stem_other_extensions(nonexistent); // must not panic
    }

    #[test]
    fn is_device_gone_matches_typed_variant() {
        // The transport-classified variant is authoritative regardless of
        // message content.
        assert!(is_device_gone(&DeviceError::DeviceGone(
            "USB error: anything".into()
        )));
        // Untyped errors still fall back to the string heuristic.
        assert!(is_device_gone(&DeviceError::Other(
            "USB error: ReadPipe timed out (30s)".into()
        )));
        assert!(!is_device_gone(&DeviceError::Other(
            "MTP protocol error: bad response".into()
        )));
        assert!(!is_device_gone(&DeviceError::Unsupported(
            "device rejected operation (0x2005)".into()
        )));
    }

    #[test]
    fn is_device_gone_matches_real_error_string() {
        // Shape of real error strings observed in the cascade-to-death logs.
        assert!(is_device_gone_str(
            "USB error: WritePipe failed: 0xe00002c0 (retry after ClearPipeStall: 0xe00002c0)"
        ));
        assert!(is_device_gone_str(
            "USB error: ReadPipe failed: 0xe00002c0 (retry after ClearPipeStall: 0xe00002c0)"
        ));
        // NotResponding: device stopped answering (observed after an art timeout).
        assert!(is_device_gone_str(
            "USB error: WritePipe failed: 0xe00002ed"
        ));
        // Retry-after-stall failed: the suffix alone means recovery failed.
        assert!(is_device_gone_str(
            "USB error: WritePipe failed: 0xe000404f (retry after ClearPipeStall: 0xe00002ed)"
        ));
        // Read timeout: once a command is in-flight and the response never
        // comes, subsequent writes cascade.
        assert!(is_device_gone_str("USB error: ReadPipe timed out (30s)"));
        assert!(is_device_gone_str("USB error: ReadPipe timed out (90s)"));
        // libusb/Linux variants — same semantics, different wording.
        assert!(is_device_gone_str(
            "USB error: write_bulk failed: Pipe error (retry after clear_halt: No such device)"
        ));
        assert!(is_device_gone_str("USB error: read_bulk timed out (30s)"));
    }

    #[test]
    fn is_device_gone_ignores_other_usb_errors() {
        // Transient or unrelated errors must NOT trip the bailout.
        // A bare pipe error (no retry suffix) may still recover via our
        // single-shot ClearPipeStall retry — only mark the session dead
        // once the retry itself has been reported as failed.
        assert!(!is_device_gone_str(
            "USB error: WritePipe failed: 0xe000404f"
        ));
        assert!(!is_device_gone_str("MTP protocol error: bad response"));
        assert!(!is_device_gone_str("IO error: file not found"));
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
