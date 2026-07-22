//! Keyboard dispatch for the TUI.
//!
//! `App::handle_key` is the single-source-of-truth dispatcher the run
//! loop feeds every [`KeyEvent`] through. Modals are tried first, in
//! priority order — each has a `handle_*_key` method that returns `true`
//! when its modal is open (an open modal consumes every key, so the
//! global map below never sees it). Only when no modal claims the event
//! does `handle_global_key` run the global key map.

use std::sync::mpsc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{
    copy_to_clipboard, App, BrowseMode, DeviceStatus, GenerationFormState, KeyOutcome, Panel,
    SidebarEntry, SidebarMode, SyncStatus,
};
use crate::audio::AudioCommand;
use crate::background::BgCommand;

impl App {
    /// Single-source-of-truth keyboard dispatcher for the TUI.
    ///
    /// Handles the same modal-then-global cascade the run loop used to inline:
    /// search, theme picker, cache-clear confirm, removal confirm, help, and
    /// track-info popup are all gated before the global match. Returns
    /// `KeyOutcome::Quit` for `q` / `Ctrl+C` so the run loop can break.
    pub fn handle_key(
        &mut self,
        key: KeyEvent,
        cmd_tx: &mpsc::Sender<BgCommand>,
        audio_tx: &mpsc::Sender<AudioCommand>,
    ) -> KeyOutcome {
        // CD import overlay. Takes priority over every other modal so the
        // user can dismiss with Esc and reach the rest of the TUI again.
        // (Unlike the modals below it deliberately lets some keys fall
        // through — e.g. Ctrl-C so the quit hatch still works.)
        if self.import_overlay.is_some() && self.handle_import_overlay_key(key) {
            return KeyOutcome::Continue;
        }

        if self.handle_playlist_name_input_key(key)
            || self.handle_generation_form_key(key)
            || self.handle_add_to_playlist_picker_key(key)
            || self.handle_playlist_delete_key(key)
            || self.handle_search_key(key)
            || self.handle_theme_picker_key(key)
            || self.handle_stem_panel_key(key)
            || self.handle_cache_clear_key(key)
            || self.handle_stem_consent_key(key)
            || self.handle_stem_bulk_confirm_key(key)
            || self.handle_removal_confirm_key(key, cmd_tx)
            || self.handle_help_key(key)
        {
            return KeyOutcome::Continue;
        }

        if self.tag_manager.is_some() {
            self.handle_tag_manager_key(key, cmd_tx);
            return KeyOutcome::Continue;
        }

        if self.handle_track_info_key(key) {
            return KeyOutcome::Continue;
        }

        if self.handle_stem_key(key) {
            return KeyOutcome::Continue;
        }

        self.handle_global_key(key, cmd_tx, audio_tx)
    }

    /// Engine-install consent overlay (opened by `M` when no stem engine
    /// is found and `[stems]` provisioning is auto). A real modal: it
    /// claims every key while open.
    fn handle_stem_consent_key(&mut self, key: KeyEvent) -> bool {
        if self.stems.consent.is_none() {
            return false;
        }
        match key.code {
            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                if let Some(consent) = self.stems.consent.take() {
                    // The install job supersedes a running batch
                    // worker-side just like a separation does — suspend
                    // it so the install's follow-on separation resumes
                    // it instead of the batch dying with a log line.
                    self.suspend_batch_for_interactive();
                    self.stems.job_gen += 1;
                    self.stems.status = super::StemStatus::Provisioning;
                    self.pending_bg_commands
                        .push(BgCommand::ProvisionStemEngine {
                            gen: self.stems.job_gen,
                            engine: consent.engine,
                            package: consent.package,
                            gpu: consent.gpu,
                        });
                }
            }
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                self.stems.consent = None;
                self.stems.pending_path = None;
                self.set_toast("Stem engine install skipped".into(), false);
            }
            _ => {}
        }
        true
    }

    /// While stem playback is Active, digit keys `1..=layout.len()` (6 or
    /// 7 per the active recipe layout) toggle stems — they normally
    /// switch sidebar modes / jump panels. Everything else falls through:
    /// digits beyond the layout (key `7` under six-stem layouts) keep
    /// their global meaning, and `M` in the global map exits stem mode.
    /// Claiming is additionally gated on the strip being visible: with
    /// the player panel hidden (`P` force-hidden, or a terminal too short
    /// for it), `1`/`2` silently mutating invisible stems read as the
    /// sidebar keys going dead.
    fn handle_stem_key(&mut self, key: KeyEvent) -> bool {
        if self.stems.status != super::StemStatus::Active || !self.stem_strip_visible() {
            return false;
        }
        match key.code {
            KeyCode::Char(c @ '1'..='8') => {
                // Claim only digits inside the active layout: key 7
                // toggles the harmony layout's 7th stem but must fall
                // through to global handling under a six-stem layout.
                let index = c as usize - '1' as usize;
                if index < self.stems.layout.map_or(0, |l| l.len()) {
                    self.toggle_stem(index);
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    /// Playlist name input modal — handles both create-new and rename.
    fn handle_playlist_name_input_key(&mut self, key: KeyEvent) -> bool {
        if self.playlist_name_input.is_none() {
            return false;
        }
        match key.code {
            KeyCode::Esc => {
                self.playlist_name_input = None;
                self.playlist_rename_target = None;
            }
            KeyCode::Enter => {
                self.commit_playlist_name_input();
            }
            KeyCode::Backspace => {
                if let Some(buf) = &mut self.playlist_name_input {
                    buf.pop();
                }
            }
            KeyCode::Char(c) => {
                if let Some(buf) = &mut self.playlist_name_input {
                    buf.push(c);
                }
            }
            _ => {}
        }
        true
    }

    /// Playlist Generation form.
    fn handle_generation_form_key(&mut self, key: KeyEvent) -> bool {
        if self.generation_form.is_none() {
            return false;
        }
        match key.code {
            KeyCode::Esc => {
                self.generation_form = None;
            }
            KeyCode::Enter => {
                self.commit_generation_form();
            }
            KeyCode::Tab => {
                if let Some(f) = &mut self.generation_form {
                    f.selected_field = (f.selected_field + 1) % GenerationFormState::FIELD_COUNT;
                }
            }
            KeyCode::BackTab => {
                if let Some(f) = &mut self.generation_form {
                    f.selected_field = if f.selected_field == 0 {
                        GenerationFormState::FIELD_COUNT - 1
                    } else {
                        f.selected_field - 1
                    };
                }
            }
            _ => {
                self.generation_form_dispatch_field_key(key);
            }
        }
        true
    }

    /// "Add this track to a playlist" picker.
    fn handle_add_to_playlist_picker_key(&mut self, key: KeyEvent) -> bool {
        if self.add_to_playlist_picker.is_none() {
            return false;
        }
        match key.code {
            KeyCode::Esc => {
                self.add_to_playlist_picker = None;
            }
            KeyCode::Enter => {
                self.confirm_add_to_playlist();
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(p) = &mut self.add_to_playlist_picker {
                    if p.selected > 0 {
                        p.selected -= 1;
                    }
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(p) = &mut self.add_to_playlist_picker {
                    if p.selected + 1 < p.options.len() {
                        p.selected += 1;
                    }
                }
            }
            _ => {}
        }
        true
    }

    /// Playlist delete confirmation.
    fn handle_playlist_delete_key(&mut self, key: KeyEvent) -> bool {
        if self.pending_playlist_delete.is_none() {
            return false;
        }
        match key.code {
            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                if let Some(id) = self.pending_playlist_delete.take() {
                    self.commit_playlist_delete(id);
                }
            }
            KeyCode::Esc | KeyCode::Char('n') => {
                self.pending_playlist_delete = None;
            }
            _ => {}
        }
        true
    }

    /// Live sidebar search (`/`).
    fn handle_search_key(&mut self, key: KeyEvent) -> bool {
        if !self.search_active {
            return false;
        }
        match key.code {
            KeyCode::Esc => {
                self.search_active = false;
                self.search_query.clear();
                self.apply_sidebar_filter();
            }
            KeyCode::Enter => {
                self.search_active = false;
                self.active_panel = Panel::Library;
            }
            KeyCode::Backspace => {
                self.search_query.pop();
                self.apply_sidebar_filter();
            }
            KeyCode::Char(c) => {
                self.search_query.push(c);
                self.apply_sidebar_filter();
            }
            _ => {}
        }
        true
    }

    /// Album bulk-separation confirmation (`M` on an album entry). A
    /// real modal: claims every key while open.
    fn handle_stem_bulk_confirm_key(&mut self, key: KeyEvent) -> bool {
        if self.stem_bulk_confirm.is_none() {
            return false;
        }
        match key.code {
            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                self.stem_bulk_confirm_accept();
            }
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                self.stem_bulk_confirm = None;
            }
            _ => {}
        }
        true
    }

    /// Theme picker overlay (`t`).
    fn handle_theme_picker_key(&mut self, key: KeyEvent) -> bool {
        if !self.show_theme_picker {
            return false;
        }
        match key.code {
            KeyCode::Esc => self.theme_picker_cancel(),
            KeyCode::Enter => self.theme_picker_confirm(),
            KeyCode::Up => self.theme_picker_move(-1),
            KeyCode::Down => self.theme_picker_move(1),
            _ => {}
        }
        true
    }

    /// Stem settings panel (`o`). A real modal: claims every key while
    /// open. The uninstall confirmation is a sub-state — `y`/Enter
    /// uninstalls the engine and evicts its model checkpoints, `Y`
    /// additionally deletes the separated-stems cache, `n`/Esc backs out.
    fn handle_stem_panel_key(&mut self, key: KeyEvent) -> bool {
        let Some(panel) = self.stem_panel.as_mut() else {
            return false;
        };
        if panel.confirm_uninstall {
            match key.code {
                KeyCode::Enter | KeyCode::Char('y') => self.stem_panel_uninstall(false),
                KeyCode::Char('Y') => self.stem_panel_uninstall(true),
                KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                    panel.confirm_uninstall = false;
                }
                _ => {}
            }
            return true;
        }
        if panel.confirm_clear_cache {
            match key.code {
                KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.stem_panel_clear_cache()
                }
                KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                    panel.confirm_clear_cache = false;
                }
                _ => {}
            }
            return true;
        }
        match key.code {
            KeyCode::Esc => self.stem_panel = None,
            KeyCode::Enter => self.stem_panel_confirm(),
            KeyCode::Up | KeyCode::Char('k') => self.stem_panel_move(-1),
            KeyCode::Down | KeyCode::Char('j') => self.stem_panel_move(1),
            KeyCode::Char('u') => self.stem_panel_request_uninstall(),
            KeyCode::Char('c') => self.stem_panel_request_clear_cache(),
            _ => {}
        }
        true
    }

    /// Playback-cache clear confirmation (`X`).
    fn handle_cache_clear_key(&mut self, key: KeyEvent) -> bool {
        if !self.pending_cache_clear {
            return false;
        }
        match key.code {
            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                self.pending_cache_clear = false;
                self.clear_playback_cache();
            }
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                self.pending_cache_clear = false;
            }
            _ => {}
        }
        true
    }

    /// Delete the on-disk playback cache and toast how much was freed.
    fn clear_playback_cache(&mut self) {
        let playback_dir = std::env::temp_dir().join("zytunes-playback");
        let mut total: u64 = 0;
        if playback_dir.exists() {
            if let Ok(entries) = std::fs::read_dir(&playback_dir) {
                for entry in entries.flatten() {
                    total += entry.metadata().map(|m| m.len()).unwrap_or(0);
                }
            }
            let _ = std::fs::remove_dir_all(&playback_dir);
        }
        if total > 0 {
            let mb = total as f64 / (1024.0 * 1024.0);
            self.set_toast(format!("Cleared {:.1} MB of cached audio", mb), false);
        } else {
            self.set_toast("Cache is already empty".into(), false);
        }
    }

    /// Device-removal confirmation (`D` in Device mode).
    fn handle_removal_confirm_key(
        &mut self,
        key: KeyEvent,
        cmd_tx: &mpsc::Sender<BgCommand>,
    ) -> bool {
        if self.pending_removal.is_none() {
            return false;
        }
        match key.code {
            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                if let Some(items) = self.pending_removal.take() {
                    let count = items.len();
                    self.set_toast(format!("Removing {} track(s) from device...", count), false);
                    let _ = cmd_tx.send(BgCommand::RemoveFromDevice(items));
                    self.removal_queue.clear();
                }
            }
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                self.pending_removal = None;
            }
            _ => {}
        }
        true
    }

    /// Help overlay (`?`).
    fn handle_help_key(&mut self, key: KeyEvent) -> bool {
        if !self.show_help {
            return false;
        }
        if let KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') = key.code {
            self.show_help = false;
        }
        true
    }

    /// Track-info popup (`I`).
    fn handle_track_info_key(&mut self, key: KeyEvent) -> bool {
        if !self.show_track_info {
            return false;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('I') => {
                self.close_track_info();
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.track_info_move(-1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.track_info_move(1);
            }
            KeyCode::PageUp => {
                self.track_info_page(-1);
            }
            KeyCode::PageDown => {
                self.track_info_page(1);
            }
            KeyCode::Char('g') => {
                self.track_info_home();
            }
            KeyCode::Char('G') => {
                self.track_info_end();
            }
            _ => {}
        }
        true
    }

    /// The global key map — everything that runs when no modal is open.
    fn handle_global_key(
        &mut self,
        key: KeyEvent,
        cmd_tx: &mpsc::Sender<BgCommand>,
        audio_tx: &mpsc::Sender<AudioCommand>,
    ) -> KeyOutcome {
        match key.code {
            KeyCode::Char('q') => {
                self.should_quit = true;
                return KeyOutcome::Quit;
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
                return KeyOutcome::Quit;
            }
            KeyCode::Char('?') => {
                self.show_help = true;
            }
            KeyCode::Char('h') => {
                self.show_keys = !self.show_keys;
            }
            KeyCode::Tab => {
                self.cycle_panel();
            }
            KeyCode::BackTab => {
                self.cycle_panel_back();
            }
            KeyCode::Char('1') => {
                self.set_sidebar_mode(SidebarMode::Artists);
            }
            KeyCode::Char('2') => {
                self.set_sidebar_mode(SidebarMode::Albums);
            }
            KeyCode::Char('4') if !self.sync.queue.is_empty() => {
                self.active_panel = Panel::SyncQueue;
            }
            KeyCode::Char('o') => {
                self.open_stem_panel();
            }
            KeyCode::Char('t') => {
                self.open_theme_picker();
            }
            KeyCode::Char('T') => {
                self.toggle_album_art_style();
            }
            KeyCode::Char('I') if self.active_panel == Panel::TrackList => {
                self.open_track_info();
            }
            KeyCode::Char('m')
                if self.browse_mode == BrowseMode::Library
                    && matches!(self.active_panel, Panel::Albums | Panel::TrackList)
                    && self.library.is_some() =>
            {
                self.open_tag_manager(cmd_tx);
            }
            KeyCode::Char('M') => {
                // On an album sidebar entry (Library browse), M means
                // "pre-warm this whole album" — or cancels the batch
                // already doing so. Everywhere else it drives the
                // per-track stem mode.
                let on_album_entry = self.browse_mode == BrowseMode::Library
                    && self.active_panel == Panel::Library
                    && matches!(
                        self.sidebar_items.get(self.sidebar_selected),
                        Some(crate::app::SidebarEntry::Album { .. })
                    );
                if on_album_entry {
                    self.press_stem_mode_on_album();
                } else {
                    self.press_stem_mode();
                }
            }
            KeyCode::Char('P') => {
                let label = self.cycle_show_player();
                self.set_toast(label.to_string(), false);
            }
            KeyCode::Char('v') => {
                // `toggle_browse_mode` already skips `Device` when nothing's
                // connected, so cycling Library → Playlists → Library always
                // works and never requires a device. The old "Connect a
                // device first" gate was a holdover from when there were
                // only two modes — drop it so users can reach Playlists
                // without plugging anything in.
                self.toggle_browse_mode();
            }
            KeyCode::Char('i') => {
                // CD import entry point. The import overlay arrives in
                // Phase 2; for now, surface the current detection status
                // so the keybinding is discoverable and the wiring is
                // exercised end-to-end.
                self.handle_cd_import_key();
            }
            // Cancel an active rip. Takes priority over the device-connect
            // binding so the user doesn't need a separate key to abort.
            KeyCode::Char('c') if self.cd.rip.is_some() => {
                self.cancel_rip();
            }
            KeyCode::Char('c') if self.device.status == DeviceStatus::Disconnected => {
                self.device.status = DeviceStatus::Detecting;
                self.connection_anim_start = Some(self.anim_frame);
                let _ = cmd_tx.send(BgCommand::Connect);
            }
            KeyCode::Char('d') => {
                self.handle_delete_key(cmd_tx);
            }
            KeyCode::Char('N') if self.browse_mode == BrowseMode::Playlists => {
                self.playlist_name_input = Some(String::new());
                self.playlist_rename_target = None;
            }
            KeyCode::Char('e') if self.browse_mode == BrowseMode::Playlists => {
                if let Some(SidebarEntry::Playlist { id, name }) =
                    self.sidebar_items.get(self.sidebar_selected).cloned()
                {
                    self.playlist_name_input = Some(name);
                    self.playlist_rename_target = Some(id);
                }
            }
            KeyCode::Char('+')
                if self.browse_mode == BrowseMode::Library
                    && self.active_panel == Panel::TrackList =>
            {
                self.open_add_to_playlist_picker();
            }
            // Open Generation form. Pre-fills with the focused track's id
            // when invoked from a Library track row, otherwise the
            // Discover-Weekly defaults.
            KeyCode::Char('G') => {
                self.open_generation_form();
            }
            // Regenerate the selected generated playlist (Playlists mode).
            // Falls through to the device-refresh handler below otherwise.
            KeyCode::Char('R') if self.browse_mode == BrowseMode::Playlists => {
                self.open_generation_form_for_regenerate();
            }
            KeyCode::Char('r') if self.device.status == DeviceStatus::Connected => {
                let _ = cmd_tx.send(BgCommand::LoadDeviceTracks);
                self.set_toast("Refreshing device tracks...".into(), false);
            }
            KeyCode::Up => {
                self.move_up();
            }
            KeyCode::Down => {
                self.move_down();
            }
            KeyCode::PageUp => {
                self.sync.log_scroll_up(10);
            }
            KeyCode::PageDown => {
                self.sync.log_scroll_down(10);
            }
            KeyCode::Right => {
                self.skip_forward();
            }
            KeyCode::Left => {
                self.skip_back();
            }
            KeyCode::Char(' ') => {
                self.toggle_playback(audio_tx);
            }
            KeyCode::Char('<') | KeyCode::Char(',') if self.now_playing.is_some() => {
                let _ = audio_tx.send(AudioCommand::Scrub { delta_ms: -5000 });
            }
            KeyCode::Char('>') | KeyCode::Char('.') if self.now_playing.is_some() => {
                let _ = audio_tx.send(AudioCommand::Scrub { delta_ms: 5000 });
            }
            KeyCode::Char('n') => {
                self.next_track(audio_tx);
            }
            KeyCode::Char('p') => {
                self.prev_track(audio_tx);
            }
            KeyCode::Enter => {
                self.handle_enter_key(cmd_tx, audio_tx);
            }
            KeyCode::Char('/') => {
                self.search_active = true;
                self.search_query.clear();
            }
            KeyCode::Char('s') if self.active_panel == Panel::TrackList => {
                self.cycle_sort();
            }
            KeyCode::Char('L') => {
                self.dump_log_to_file();
            }
            KeyCode::Char('S') if !self.sync.queue.is_empty() => {
                self.active_panel = Panel::SyncQueue;
                self.execute_sync(cmd_tx);
            }
            KeyCode::Char('a') => {
                self.handle_add_key();
            }
            KeyCode::Char('A') => {
                if self.browse_mode == BrowseMode::Device {
                    self.queue_device_removal();
                } else if self.active_panel == Panel::TrackList {
                    self.add_all_visible_to_queue();
                }
            }
            KeyCode::Char('D')
                if self.browse_mode == BrowseMode::Device && !self.removal_queue.is_empty() =>
            {
                self.pending_removal = Some(self.removal_queue.clone());
            }
            KeyCode::Char('C') => {
                self.handle_clear_key();
            }
            KeyCode::Char('U') if self.browse_mode == BrowseMode::Device => {
                self.dedupe_device(cmd_tx);
            }
            KeyCode::Char('X') => {
                self.pending_cache_clear = true;
            }
            KeyCode::Esc => {
                self.handle_escape_key(cmd_tx);
            }
            _ => {}
        }

        KeyOutcome::Continue
    }

    /// `1`/`2` — switch the sidebar between Artists and Albums.
    fn set_sidebar_mode(&mut self, mode: SidebarMode) {
        self.save_sidebar_pos();
        self.sidebar_mode = mode;
        self.refresh_sidebar();
        self.active_panel = Panel::Library;
    }

    /// `d` — panel-dependent delete: dequeue a sync item or delete a
    /// playlist / playlist track. On any panel that doesn't claim `d`
    /// for itself, it disconnects the device — disconnecting shouldn't
    /// require tabbing over to the Device panel first.
    fn handle_delete_key(&mut self, cmd_tx: &mpsc::Sender<BgCommand>) {
        match self.active_panel {
            Panel::SyncQueue => {
                self.remove_queue_item();
                return;
            }
            Panel::Library if self.browse_mode == BrowseMode::Playlists => {
                if let Some(SidebarEntry::Playlist { id, .. }) =
                    self.sidebar_items.get(self.sidebar_selected).cloned()
                {
                    self.pending_playlist_delete = Some(id);
                }
                return;
            }
            Panel::TrackList if self.browse_mode == BrowseMode::Playlists => {
                self.remove_selected_track_from_playlist();
                return;
            }
            _ => {}
        }
        if self.device.status != DeviceStatus::Disconnected {
            self.disconnect_device(cmd_tx);
        }
    }

    /// Disconnect the device: drop the worker session and clear all
    /// device-side UI state.
    fn disconnect_device(&mut self, cmd_tx: &mpsc::Sender<BgCommand>) {
        let _ = cmd_tx.send(BgCommand::Disconnect);
        self.device.status = DeviceStatus::Disconnected;
        self.device.name = None;
        self.device.tracks.clear();
        self.clear_device_index();
        self.set_toast("Disconnected".into(), false);
    }

    /// `Enter` — panel-dependent activate: drill into the sidebar
    /// selection, play the selected track, or start the queued sync.
    fn handle_enter_key(
        &mut self,
        cmd_tx: &mpsc::Sender<BgCommand>,
        audio_tx: &mpsc::Sender<AudioCommand>,
    ) {
        match self.active_panel {
            Panel::Library => {
                self.select_sidebar_item();
                if self.has_album_browser() {
                    self.active_panel = Panel::Albums;
                } else {
                    self.active_panel = Panel::TrackList;
                }
            }
            Panel::Albums => {
                self.active_panel = Panel::TrackList;
            }
            Panel::TrackList => {
                self.play_selected_track(audio_tx);
            }
            Panel::SyncQueue => {
                self.execute_sync(cmd_tx);
            }
            _ => {}
        }
    }

    /// `a` — queue for removal in Device mode, otherwise add the current
    /// selection (track, sidebar item, or visible album) to the sync queue.
    fn handle_add_key(&mut self) {
        if self.browse_mode == BrowseMode::Device {
            self.queue_device_removal();
        } else {
            match self.active_panel {
                Panel::TrackList => {
                    self.add_selected_track_to_queue();
                }
                Panel::Library => {
                    self.add_sidebar_item_to_queue();
                }
                Panel::Albums => {
                    self.add_all_visible_to_queue();
                }
                _ => {}
            }
        }
    }

    /// `C` — clear the removal queue (Device mode) or the sync queue.
    fn handle_clear_key(&mut self) {
        if self.browse_mode == BrowseMode::Device {
            let count = self.removal_queue.len();
            self.clear_removal_queue();
            if count > 0 {
                self.set_toast(format!("Cleared {} queued removal(s)", count), false);
            }
        } else if self.active_panel == Panel::SyncQueue {
            self.clear_queue();
        }
    }

    /// `Esc` — cancel a running sync and dismiss any toast.
    fn handle_escape_key(&mut self, cmd_tx: &mpsc::Sender<BgCommand>) {
        if matches!(self.sync.status, SyncStatus::Running { .. }) {
            let _ = cmd_tx.send(BgCommand::CancelSync);
            // The user is bailing on the sync — drop any
            // playlist-creation follow-ups so they don't fire on
            // whatever partial sync did finish.
            self.pending_playlist_imports.clear();
        }
        self.toast_message = None;
    }

    /// `L` — dump the sync log to `/tmp/zytunes-log.txt` and copy the
    /// path to the system clipboard.
    fn dump_log_to_file(&mut self) {
        let path = std::path::PathBuf::from("/tmp/zytunes-log.txt");
        let content = self.sync.log.join("\n");
        match std::fs::write(&path, &content) {
            Ok(_) => {
                let path_str = path.display().to_string();
                let toast = match copy_to_clipboard(&path_str) {
                    Ok(()) => {
                        format!("Log dumped to {} (copied to clipboard)", path_str)
                    }
                    Err(e) => {
                        format!("Log dumped to {} (clipboard: {})", path_str, e)
                    }
                };
                self.set_toast(toast, false);
            }
            Err(e) => self.set_toast(format!("Log dump failed: {}", e), true),
        }
    }
}
