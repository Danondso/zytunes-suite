//! Background-event dispatch for the TUI.
//!
//! `App::handle_bg_event` is the single entry point the run loop feeds
//! [`BgEvent`]s through. The match itself stays a thin dispatcher — every
//! multi-line reaction lives in a named `on_*` method below so each
//! event's behaviour reads in isolation. Trivial field-set arms stay
//! inline; anything with ordering constraints or follow-up commands gets
//! its own method and doc comment.

use zytunes::mtp::parse::DeviceEntry;

use super::{App, BrowseMode, DeviceStatus, RipProgressState, StemStatus, SyncStatus};
use crate::audio::AudioCommand;
use crate::background::{BgCommand, BgEvent, DeviceInfo, StorageInfo};

impl App {
    pub fn handle_bg_event(&mut self, event: BgEvent) {
        match event {
            BgEvent::LibraryLoaded(result) => self.on_library_loaded(result),
            BgEvent::LibraryScanProgress(p) => self.on_library_scan_progress(p),
            BgEvent::DeviceDetected(info) => self.on_device_detected(info),
            BgEvent::SessionReady(storage) => self.on_session_ready(storage),
            BgEvent::SessionFailed(e) => self.on_session_failed(e),
            BgEvent::DeviceSyncStatus(status) => {
                self.device.sync_status = status;
            }
            BgEvent::AlbumArtLoaded { key, image } => {
                // Only apply if the key still matches (user hasn't navigated away).
                if key == self.album_art_key {
                    self.album_art = image;
                    self.album_art_cache = None;
                    self.album_art_size = (0, 0);
                }
            }
            BgEvent::LoadingDeviceTracks => {
                self.device.loading_tracks = true;
                self.set_toast("Loading device tracks...".into(), false);
            }
            BgEvent::DeviceTracksLoaded(tracks) => self.on_device_tracks_loaded(tracks),
            BgEvent::DeviceTrackAdded(entry) => {
                self.device.add_indexed_track(&entry);
                self.device.tracks.push(entry);
                self.device_index_dirty = true;
            }
            BgEvent::DeviceTrackRemoved(path) => self.on_device_track_removed(path),
            BgEvent::Error(e) => {
                self.device.loading_tracks = false;
                self.set_toast(e, true);
            }
            BgEvent::SyncMessage(msg) => {
                self.sync.log.push(msg);
            }
            BgEvent::SyncProgress {
                current,
                total,
                track_name,
            } => {
                self.sync.status = SyncStatus::Running { current, total };
                self.sync.current_track = track_name;
            }
            BgEvent::SyncTrackDone {
                track_name,
                success,
                error,
            } => {
                if !success {
                    self.set_toast(
                        format!("Failed: {} - {}", track_name, error.unwrap_or_default()),
                        true,
                    );
                }
            }
            BgEvent::SyncComplete {
                success,
                failed,
                skipped,
            } => self.on_sync_complete(success, failed, skipped),
            BgEvent::RemoveProgress {
                current,
                total,
                name,
            } => {
                self.sync.status = SyncStatus::Running { current, total };
                self.sync.current_track = format!("Removing: {}", name);
            }
            BgEvent::RemoveComplete { success, failed } => {
                self.sync.status = SyncStatus::Idle;
                self.set_toast(
                    format!("Removed {} tracks, {} failed", success, failed),
                    failed > 0,
                );
            }
            BgEvent::StorageUpdated(storage) => {
                self.device.storage = Some(storage);
            }
            BgEvent::PhotoSyncComplete { success, failed } => {
                if success > 0 || failed > 0 {
                    self.set_toast(
                        format!("Photo sync: {} done, {} failed", success, failed),
                        failed > 0,
                    );
                }
            }
            BgEvent::VideoSyncComplete { success, failed } => {
                if success > 0 || failed > 0 {
                    self.set_toast(
                        format!("Video sync: {} done, {} failed", success, failed),
                        failed > 0,
                    );
                }
            }
            BgEvent::AcquiredItemsCount(count) => {
                self.device.acquired_items = count;
            }
            BgEvent::PlaylistImported { name, summary } => self.on_playlist_imported(name, summary),
            BgEvent::CdStatus(status) => {
                self.cd.detect_in_flight = false;
                self.cd.last_status = Some(status);
            }
            BgEvent::RipEvent(event) => {
                self.handle_rip_event(event);
            }
            BgEvent::MbSearchResults { token, result } => {
                self.handle_mb_search_results(token, result);
            }
            BgEvent::MbReleaseLoaded { token, result } => {
                self.handle_mb_release_loaded(token, result);
            }
            BgEvent::TagsApplied {
                token,
                results,
                rename_map,
            } => {
                self.handle_tags_applied(token, results, rename_map);
            }
            BgEvent::LibraryRereadComplete { token, result } => {
                self.handle_library_reread_complete(token, result);
            }
            BgEvent::AcoustIdResolved { token, result } => {
                self.handle_acoustid_resolved(token, result);
            }
            BgEvent::MbRecordingReleases { token, result } => {
                self.handle_mb_recording_releases(token, result);
            }
            BgEvent::StemEngineProgress(line) => {
                self.sync.log.push(format!("[stems] {line}"));
            }
            BgEvent::StemEngineReady { gen, command } => self.on_stem_engine_ready(gen, command),
            BgEvent::StemEngineFailed {
                gen,
                error,
                cancelled,
            } => self.on_stem_engine_failed(gen, error, cancelled),
            BgEvent::StemProgress { gen, pct, .. } => {
                if gen == self.stems.job_gen {
                    if let super::StemStatus::Separating { pct: ref mut p } = self.stems.status {
                        *p = Some(pct);
                    }
                }
            }
            BgEvent::StemsReady {
                gen,
                track_path,
                stems,
            } => {
                self.on_stems_ready(gen, track_path, *stems);
                self.maybe_resume_stem_batch();
            }
            BgEvent::StemsFailed {
                gen,
                track_path,
                error,
                cancelled,
            } => {
                self.on_stems_failed(gen, track_path, error, cancelled);
                self.maybe_resume_stem_batch();
            }
            BgEvent::StemEngineUninstalled {
                engine,
                reclaimed_bytes,
                error,
            } => self.on_stem_engine_uninstalled(engine, reclaimed_bytes, error),
            BgEvent::StemBatchProgress {
                gen,
                current,
                total,
                pct,
            } => {
                if let Some(batch) = self.stems_batch.as_mut() {
                    if gen == batch.gen {
                        batch.current = current;
                        batch.total = total;
                        batch.pct = pct;
                    }
                }
            }
            BgEvent::StemBatchDone {
                gen,
                separated,
                skipped,
                failed,
                cancelled,
            } => self.on_stem_batch_done(gen, separated, skipped, failed, cancelled),
        }
    }

    /// Engine uninstall finished: surface the outcome and — on success
    /// only — clear a `[stems] command` that pointed at the removed
    /// engine (in memory and in config.toml).
    fn on_stem_engine_uninstalled(
        &mut self,
        engine: zytunes::stems::provision::EngineKind,
        reclaimed_bytes: u64,
        error: Option<String>,
    ) {
        if self.stem_engine_uninstalled_in_memory(engine, reclaimed_bytes, error) {
            crate::config::update(|c| c.stems.command = None);
        }
    }

    /// In-memory half of [`Self::on_stem_engine_uninstalled`] (the
    /// `cycle_show_player` pattern). Returns whether `[stems] command`
    /// was cleared and should be persisted: a stale path there is exactly
    /// what would suppress the reinstall consent prompt later, but a
    /// command belonging to the *other* engine must survive — and so
    /// must the command after a FAILED uninstall (uv missing, non-uv
    /// install), where the engine is still on disk and forgetting its
    /// path would orphan a working install.
    pub(crate) fn stem_engine_uninstalled_in_memory(
        &mut self,
        engine: zytunes::stems::provision::EngineKind,
        reclaimed_bytes: u64,
        error: Option<String>,
    ) -> bool {
        let mb = reclaimed_bytes as f64 / (1024.0 * 1024.0);
        let failed = error.is_some();
        match error {
            Some(e) => {
                self.sync.log.push(format!("[stems] uninstall: {e}"));
                self.set_toast(format!("Uninstall failed: {e}"), true);
            }
            None => {
                self.set_toast(
                    format!("Uninstalled {} — reclaimed {mb:.1} MB", engine.exe_name()),
                    false,
                );
            }
        }
        let matches_engine = !failed
            && self
                .stems_cfg
                .command
                .as_deref()
                .and_then(|c| {
                    std::path::Path::new(c)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| n.contains(engine.exe_name()))
                })
                .unwrap_or(false);
        if matches_engine {
            self.stems_cfg.command = None;
        }
        // Refresh the panel's engine rows if it's open (reopened panels
        // re-resolve anyway; this keeps a visible one honest).
        if self.stem_panel.is_some() {
            self.open_stem_panel();
        }
        matches_engine
    }

    /// Engine install finished: persist the resolved path (in memory and
    /// to config.toml) and resume the separation the user asked for.
    fn on_stem_engine_ready(&mut self, gen: u64, command: std::path::PathBuf) {
        let path_str = self.stem_engine_ready_in_memory(gen, &command);
        crate::config::update(move |c| c.stems.command = Some(path_str));
    }

    /// In-memory half of [`Self::on_stem_engine_ready`], split out (the
    /// `cycle_show_player` pattern) so tests can drive the state machine
    /// without writing the developer's real config file. Returns the
    /// engine path string the wrapper persists — even for a stale `gen`,
    /// since a finished install is a usable engine no matter which job
    /// ran it; staleness only skips the state transitions.
    pub(crate) fn stem_engine_ready_in_memory(
        &mut self,
        gen: u64,
        command: &std::path::Path,
    ) -> String {
        let path_str = command.to_string_lossy().into_owned();
        self.sync
            .log
            .push(format!("[stems] engine ready at {path_str}"));
        self.stems_cfg.command = Some(path_str.clone());
        if gen != self.stems.job_gen {
            return path_str;
        }
        if let Some(pending) = self.stems.pending_path.take() {
            let recipe = self.stems_recipe();
            self.dispatch_separation(pending, recipe);
        } else {
            // Track changed while installing: nothing to auto-split, but
            // the engine is now ready for the next M-press.
            self.stems.reset();
            self.sync
                .log
                .push("[stems] press M to split the playing track".into());
        }
        path_str
    }

    fn on_stem_engine_failed(&mut self, gen: u64, err: String, cancelled: bool) {
        if cancelled {
            // User-initiated (or superseded) — not an error. The cancel
            // site already reset state and toasted "Stem job cancelled".
            self.sync
                .log
                .push(format!("[stems] engine install cancelled: {err}"));
            return;
        }
        self.sync
            .log
            .push(format!("[stems] engine install failed: {err}"));
        if gen != self.stems.job_gen {
            return;
        }
        self.stems.reset();
        self.stems.pending_path = None;
        self.set_toast(format!("Stem engine install failed: {err}"), true);
    }

    /// Terminal event for an album batch. A suspended batch's
    /// cancelled-Done is the supersede completing — the state survives
    /// (banking this round's separations) and the interactive job's own
    /// terminal event resumes it. A stale gen (batch already
    /// cancelled/replaced app-side) is ignored.
    fn on_stem_batch_done(
        &mut self,
        gen: u64,
        separated: usize,
        skipped: usize,
        failed: usize,
        cancelled: bool,
    ) {
        let Some(batch) = self.stems_batch.as_mut() else {
            return;
        };
        if gen != batch.gen {
            return;
        }
        if cancelled && batch.suspended {
            batch.separated_so_far += separated;
            self.sync.log.push(format!(
                "[stems] batch paused ({separated} done so far this round)"
            ));
            return;
        }
        let album = batch.album.clone();
        let prior = batch.separated_so_far;
        self.stems_batch = None;
        if cancelled {
            self.sync.log.push(format!(
                "[stems] batch cancelled ({} separated)",
                prior + separated
            ));
            return;
        }
        // The final round re-counts tracks separated before a suspend as
        // cache hits — shift them back so the toast reports true work.
        let total_separated = prior + separated;
        let already_cached = skipped.saturating_sub(prior);
        let mut parts = vec![format!("{total_separated} separated")];
        if already_cached > 0 {
            parts.push(format!("{already_cached} already cached"));
        }
        if failed > 0 {
            parts.push(format!("{failed} failed"));
        }
        self.set_toast(
            format!("Stems for {album}: {}", parts.join(", ")),
            failed > 0,
        );
    }

    /// Separation finished. If we're still waiting on this exact track and
    /// it's still the one playing, swap playback into the stem mixer at
    /// the current position; otherwise the stems just sit in the cache
    /// (instant on the next `M`-press for that track).
    fn on_stems_ready(&mut self, gen: u64, track_path: String, stems: zytunes::stems::StemSet) {
        let waiting =
            gen == self.stems.job_gen && matches!(self.stems.status, StemStatus::Separating { .. });
        let still_playing = self.playing_track_path().as_deref() == Some(track_path.as_str());
        if !(waiting && still_playing) {
            self.sync
                .log
                .push(format!("[stems] stems cached for {track_path}"));
            if waiting {
                self.stems.reset();
            }
            return;
        }

        self.stems.enabled = [true; zytunes::stems::MAX_STEMS];
        let gains = zytunes::stems::new_stem_gains(&self.stems.enabled[..stems.layout.len()]);
        self.stems.layout = Some(stems.layout);
        // One gapless swap: the audio thread pre-seeks the mixer to its
        // own live position (preserving pause state) before cutting over,
        // so no Scrub/Pause choreography is needed here.
        self.pending_audio_commands.push(AudioCommand::SwapSource {
            target: crate::audio::SwapTarget::Stems {
                stems: Box::new(stems),
                gains: gains.clone(),
            },
        });
        self.stems.gains = Some(gains);
        self.stems.status = StemStatus::Active;
        self.set_toast(
            format!(
                "Stem mode — 1-{} toggle stems, M exits",
                self.stems.layout.map_or(6, |l| l.len())
            ),
            false,
        );
    }

    fn on_stems_failed(&mut self, gen: u64, track_path: String, error: String, cancelled: bool) {
        // Identity is the job generation, NOT the track path: a cancelled
        // job's late terminal event for track A must not reset a newer
        // job separating that same track A.
        let ours = gen == self.stems.job_gen;
        if cancelled {
            self.sync
                .log
                .push(format!("[stems] separation of {track_path} cancelled"));
        } else {
            self.sync.log.push(format!(
                "[stems] separation of {track_path} failed: {error}"
            ));
            // A missing Python module means the engine env itself is
            // broken (e.g. installed before the `--with numpy` fix).
            // Discovery keeps finding the broken shim and skips
            // provisioning, so point at the manual repair.
            if error.contains("ModuleNotFoundError") {
                self.sync.log.push(
                    "[stems] engine env looks broken — run `uv tool uninstall demucs`, \
                     clear [stems] command in config.toml, then press M to reinstall"
                        .into(),
                );
            }
            if ours {
                self.set_toast(format!("Stem separation failed: {error}"), true);
            }
        }
        if ours {
            self.stems.reset();
        }
    }

    /// The background library scan finished (successfully or not).
    fn on_library_loaded(
        &mut self,
        result: Result<Box<dyn zytunes::library::MusicLibrary + Send>, String>,
    ) {
        self.loading_library = false;
        self.scan_progress = None;
        self.scan_phrase = None;
        self.scan_phrase_rotated_at = None;
        self.scan_samples.clear();
        match result {
            Ok(lib) => {
                self.library = Some(lib);
                // Re-resolve the popup's cached lib `Track` against
                // the new library *before* `refresh_sidebar` clears
                // `track_list` — closed popups drop the cache, open
                // ones repaint from the freshly-scanned metadata.
                self.refresh_track_info_lib();
                self.rebuild_artist_device_status();
                self.refresh_sidebar();
                // If a device connected before the library finished
                // scanning, the merge couldn't fire on
                // `DeviceTracksLoaded`. Catch up now.
                self.merge_device_plays_into_local();
            }
            Err(e) => {
                self.set_toast(format!("Library: {}", e), true);
            }
        }
    }

    fn on_library_scan_progress(&mut self, p: zytunes::dirlib::ScanProgress) {
        self.scan_progress = Some((p.completed, p.total));
        if let Some(sample) = p.sample {
            // Keep a small rolling buffer (~128 most recent).
            if self.scan_samples.len() >= 128 {
                self.scan_samples.remove(0);
            }
            self.scan_samples.push(sample);
        }
        self.maybe_rotate_scan_phrase();
    }

    fn on_device_detected(&mut self, info: DeviceInfo) {
        self.device.name = Some(info.name);
        self.device.firmware = info.firmware_version;
        self.device.serial = info.serial_number;
        self.device.manufacturer = info.manufacturer;
        self.device.model = info.model;
        self.device.usb_mode = info.usb_mode;
        self.device.family = Some(info.family);
        self.device.status = DeviceStatus::Connecting;
    }

    fn on_session_ready(&mut self, storage: Option<StorageInfo>) {
        self.device.status = DeviceStatus::Connected;
        self.connection_anim_start = None;
        self.device.storage = storage;
        self.set_toast("Device connected".into(), false);

        // Auto-sync photos/videos if configured.
        let cfg = crate::config::load();
        if let Some(photo_dir) = std::env::var("ZYTUNES_PHOTOS_DIR").ok().or(cfg.photo_dir) {
            self.pending_bg_commands
                .push(BgCommand::SyncPhotos { dir: photo_dir });
        }
        if let Some(video_dir) = std::env::var("ZYTUNES_VIDEOS_DIR").ok().or(cfg.video_dir) {
            self.pending_bg_commands
                .push(BgCommand::SyncVideos { dir: video_dir });
        }
    }

    fn on_session_failed(&mut self, e: String) {
        self.device.status = DeviceStatus::Disconnected;
        self.device.sync_status = None;
        self.connection_anim_start = None;
        self.set_toast(format!("Connection failed: {}", e), true);
        if self.browse_mode == BrowseMode::Library {
            self.retag_on_device();
        }
    }

    fn on_device_tracks_loaded(&mut self, tracks: Vec<DeviceEntry>) {
        self.device.loading_tracks = false;
        self.device.tracks = tracks;
        self.build_device_index();
        self.rebuild_artist_device_status();
        self.set_toast(
            format!("Loaded {} device tracks", self.device.tracks.len()),
            false,
        );
        if self.browse_mode == BrowseMode::Device {
            self.refresh_sidebar();
        }
        if self.browse_mode == BrowseMode::Library {
            self.retag_on_device();
        }
        // Fold the device's per-track play/skip counters into the
        // aggregate local-plays sidecar via per-(track, device)
        // baselines. No-op when the library hasn't loaded yet —
        // the `LibraryLoaded` handler runs the merge in that order.
        self.merge_device_plays_into_local();
    }

    fn on_device_track_removed(&mut self, path: String) {
        // path is a full device path like "/Music/Artist/Album/track.mp3"
        // but DeviceEntry.name is relative like "Artist/Album/track.mp3"
        let relative = path.strip_prefix("/Music/").unwrap_or(&path).to_string();
        self.device.tracks.retain(|t| t.name != relative);
        self.device.remove_indexed_track(&relative);
        self.device_index_dirty = true;
    }

    /// A sync run finished. Clears the queue, reports totals, then drains
    /// any queued playlist imports now that the tracks they reference are
    /// on the device.
    fn on_sync_complete(&mut self, success: usize, failed: usize, skipped: usize) {
        self.sync.status = SyncStatus::Idle;
        self.sync.queue.clear();
        self.sync.queue_selected = 0;
        let msg = if skipped > 0 {
            format!(
                "Sync complete: {} done, {} skipped, {} failed",
                success, skipped, failed
            )
        } else {
            format!("Sync complete: {} done, {} failed", success, failed)
        };
        self.set_toast(msg, failed > 0 || skipped > 0);
        // Drain queued playlist imports — the file sync completed,
        // so the device-side resolver will now see freshly-uploaded
        // tracks. Per-backend gating decides whether each spec
        // actually fires:
        //   - Zune: always fire (validated end-to-end on hw 2026-04-26)
        //   - iPod: only fire when ZYTUNES_EXPERIMENTAL_PLAYLIST_SYNC=1
        //     until the iTunesDB-corruption incident is root-caused
        //   - Unknown family: skip (defensive — no point sending to
        //     a backend whose Err semantics we don't trust yet)
        let family = self.device.family;
        let allow = match family {
            Some(zytunes::device::DeviceFamily::Zune) => true,
            Some(zytunes::device::DeviceFamily::Ipod) => self.experimental_playlist_sync,
            None => false,
        };
        let drained: Vec<_> = self.pending_playlist_imports.drain(..).collect();
        for spec in drained {
            if allow {
                self.sync
                    .log
                    .push(format!("Importing playlist \"{}\" to device", spec.name));
                self.pending_bg_commands.push(BgCommand::ImportPlaylist {
                    name: spec.name,
                    track_keys: spec.track_keys,
                });
            } else {
                let reason = match family {
                    Some(zytunes::device::DeviceFamily::Ipod) => {
                        "iPod playlist sync gated off after 2026-04-26 \
                         iTunesDB-corruption incident; set \
                         ZYTUNES_EXPERIMENTAL_PLAYLIST_SYNC=1 to opt in"
                    }
                    _ => "no device session active",
                };
                self.sync.log.push(format!(
                    "Playlist \"{}\": device-side push skipped ({reason})",
                    spec.name
                ));
            }
        }
    }

    fn on_playlist_imported(
        &mut self,
        name: String,
        summary: Result<zytunes::mtp::PlaylistImportSummary, String>,
    ) {
        match summary {
            Ok(s) => {
                let verb = if s.replaced { "Replaced" } else { "Created" };
                let msg = if s.skipped > 0 {
                    format!(
                        "{verb} playlist \"{}\" on device — {} tracks ({} unresolved)",
                        name, s.resolved, s.skipped
                    )
                } else {
                    format!(
                        "{verb} playlist \"{}\" on device — {} tracks",
                        name, s.resolved
                    )
                };
                self.sync.log.push(msg.clone());
                self.set_toast(msg, false);
            }
            Err(e) => {
                let msg = format!("Playlist \"{}\" import failed: {}", name, e);
                self.sync.log.push(msg.clone());
                self.set_toast(msg, true);
            }
        }
    }

    fn handle_rip_event(&mut self, event: crate::background::RipEvent) {
        use crate::background::RipEvent;
        match event {
            RipEvent::Started {
                current,
                total,
                track_title,
                track_length_ms,
            } => {
                let errors = self
                    .cd
                    .rip
                    .as_ref()
                    .map(|r| r.errors.clone())
                    .unwrap_or_default();
                self.cd.rip = Some(RipProgressState {
                    current,
                    total,
                    track_title,
                    track_length_ms,
                    elapsed_ms: 0,
                    errors,
                });
            }
            RipEvent::Progress { elapsed_ms } => {
                if let Some(rip) = self.cd.rip.as_mut() {
                    rip.elapsed_ms = elapsed_ms;
                }
            }
            RipEvent::TrackDone { track_title, error } => {
                if let Some(rip) = self.cd.rip.as_mut() {
                    if let Some(e) = error {
                        rip.errors.push(format!("{track_title}: {e}"));
                    }
                }
            }
            RipEvent::Complete {
                ripped,
                failed,
                cancelled,
                ejected,
            } => {
                let errors = self
                    .cd
                    .rip
                    .as_ref()
                    .map(|r| r.errors.clone())
                    .unwrap_or_default();
                self.cd.rip = None;
                let mut msg = if cancelled {
                    format!("Rip cancelled: {ripped} ripped, {failed} failed")
                } else if failed > 0 {
                    format!("Rip done: {ripped} ripped, {failed} failed")
                } else {
                    format!("Rip complete: {ripped} tracks ripped")
                };
                if ejected {
                    msg.push_str(", disc ejected");
                }
                let is_error = failed > 0 || cancelled;
                self.set_toast(msg, is_error);
                for e in errors {
                    self.sync.log.push(format!("[rip] {e}"));
                }
                // Refresh the library so the newly-ripped tracks become
                // visible in the sidebar/track list. Re-uses the same
                // `LoadLibrary` path the initial startup load takes — the
                // dirlib cache is mtime-keyed so the re-scan only re-parses
                // the new files (existing entries hit the cache). Skipped
                // when zero tracks landed so a fully-cancelled or fully-
                // failed rip doesn't trigger a no-op scan.
                if ripped > 0 {
                    self.queue_library_reload();
                }
            }
        }
    }
}
