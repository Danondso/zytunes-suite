//! Playlist UI state types: pending-import queue, the "add to playlist"
//! picker, and the Generation form. None of these touch `App` directly —
//! the dispatcher methods that mutate them stay in `app.rs` because they
//! reach into many other App fields besides the playlist slice.
//!
//! Also hosts the standalone helpers that the form/name logic depends on:
//! `device_playlist_sync_enabled` (env-var gate) and `short_date` (the
//! YYYY-MM-DD stamp default-named playlists pick up).

use zytunes::playlist::{GenerationParams, Playlist, PlaylistKind, SeedStrategy};

/// One entry in `App::pending_playlist_imports`. Carries the playlist's
/// display name plus its ordered `(artist, album, title)` tuples so the
/// background worker can resolve them against the device's current track
/// set after the file sync lands.
#[derive(Debug, Clone)]
pub struct PendingPlaylistImport {
    pub name: String,
    pub track_keys: Vec<(String, String, String)>,
}

/// State for the "add this track to a playlist" overlay (opened with `+` on
/// a Library track row). Built when the user invokes the action; dropped on
/// confirm/cancel.
#[derive(Debug, Clone)]
pub struct AddToPlaylistPicker {
    /// Library `Track::id` being added.
    pub track_id: u64,
    /// Display name shown in the popup header.
    pub track_label: String,
    /// Snapshot of `(playlist_id, name)` at popup-open time so renames
    /// during the popup's lifetime don't shift selection.
    pub options: Vec<(u64, String)>,
    pub selected: usize,
}

/// In-progress state for the "Generate Playlist" modal.
///
/// Fields mirror [`GenerationParams`] one-to-one but live as primitives the
/// modal can edit incrementally. `commit()` (in the App impl) builds a
/// `GenerationParams` + name pair and hands it off to the recommender.
///
/// `selected_field` is the cursor — index into the field list rendered by
/// `draw_generation_form_overlay`. Tab/Shift-Tab move it. Field types know
/// what input keys mean for them (text accepts chars, radios respond to
/// up/down, sliders to `<` / `>`, checkboxes to space).
#[derive(Debug, Clone)]
pub struct GenerationFormState {
    pub name: String,
    pub seed_strategy_idx: usize, // 0=TopPlayed 1=Recent 2=Track 3=Artist 4=Genre
    pub window_days: u32,
    pub seed_count: usize,
    pub target_length: usize,
    pub max_per_artist: usize,
    pub max_per_album: usize,
    /// Slider 0..1, applied to `GenerationParams::diversity_lambda_bits`.
    pub diversity: f32,
    /// Slider 0..1, applied to `GenerationParams::novelty_bits`.
    pub novelty: f32,
    pub exclude_on_device: bool,
    /// Optional explicit context for `Track`/`Artist`/`Genre` strategies —
    /// captured at form-open time when the user invoked from a track row
    /// (`R` on a track) so the strategy can be applied without re-resolving.
    pub seed_context: SeedContext,
    /// `Some(playlist_id)` means we're regenerating that playlist in place;
    /// `None` means a brand-new generated playlist.
    pub regenerating: Option<u64>,
    pub selected_field: usize,
}

/// Captured context for non-behavioral seed strategies. Filled when the
/// form is opened with a specific seed (e.g. `R` on a track row); ignored
/// for the behavioral strategies.
#[derive(Debug, Clone, Default)]
pub struct SeedContext {
    pub track_id: Option<u64>,
    pub artist: Option<String>,
    pub genre: Option<String>,
}

impl GenerationFormState {
    /// Total form fields the cursor can land on. Kept in one place so the
    /// dispatcher and the renderer stay in sync.
    pub const FIELD_COUNT: usize = 10;
    pub const FIELD_NAME: usize = 0;
    pub const FIELD_SEED_STRATEGY: usize = 1;
    pub const FIELD_WINDOW_DAYS: usize = 2;
    pub const FIELD_SEED_COUNT: usize = 3;
    pub const FIELD_TARGET_LEN: usize = 4;
    pub const FIELD_MAX_PER_ARTIST: usize = 5;
    pub const FIELD_MAX_PER_ALBUM: usize = 6;
    pub const FIELD_DIVERSITY: usize = 7;
    pub const FIELD_NOVELTY: usize = 8;
    pub const FIELD_EXCLUDE_DEVICE: usize = 9;

    /// Build a fresh form pre-filled from the Discover Weekly defaults plus
    /// a date-stamped name. Use [`Self::for_track_seed`] when the user
    /// invoked from `R` on a track row.
    pub fn discover_weekly_default(now_ms: u64) -> Self {
        let p = GenerationParams::default_discover_weekly();
        GenerationFormState {
            name: format!("Discover Weekly · {}", short_date(now_ms)),
            seed_strategy_idx: match &p.seed_strategy {
                SeedStrategy::TopPlayed { .. } => 0,
                SeedStrategy::RecentlyPlayed { .. } => 1,
                SeedStrategy::Track(_) => 2,
                SeedStrategy::Artist(_) => 3,
                SeedStrategy::Genre(_) => 4,
            },
            window_days: 30,
            seed_count: 10,
            target_length: p.target_length,
            max_per_artist: p.max_per_artist,
            max_per_album: p.max_per_album,
            diversity: p.diversity_lambda(),
            novelty: p.novelty(),
            exclude_on_device: p.exclude_on_device,
            seed_context: SeedContext::default(),
            regenerating: None,
            selected_field: 0,
        }
    }

    /// Form pre-filled with the available seed contexts (track id, artist
    /// name, genre tag) so the user can flip between Track / Artist / Genre
    /// strategies in the form without having to re-invoke from a different
    /// row. `default_strategy_idx` picks which radio option lights up first
    /// — pass 2 for Track, 3 for Artist, 4 for Genre.
    ///
    /// `label` drives the default playlist name. Pass the track title for
    /// Track-seeded forms, the artist for Artist-seeded forms, etc.
    pub fn for_library_context(
        now_ms: u64,
        track_id: Option<u64>,
        artist: Option<String>,
        genre: Option<String>,
        default_strategy_idx: usize,
        label: &str,
    ) -> Self {
        let mut s = Self::discover_weekly_default(now_ms);
        s.seed_strategy_idx = default_strategy_idx.min(4);
        s.seed_context = SeedContext {
            track_id,
            artist,
            genre,
        };
        s.name = format!("More like {} · {}", label, short_date(now_ms));
        s
    }

    /// Convenience: the existing "more like this track" shortcut. Equivalent
    /// to `for_library_context(now, Some(id), None, None, 2, label)`.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn for_track_seed(now_ms: u64, track_id: u64, label: &str) -> Self {
        Self::for_library_context(now_ms, Some(track_id), None, None, 2, label)
    }

    /// Returns `Ok(())` when the currently-selected strategy has the
    /// context fields it needs to actually generate, or `Err(message)`
    /// describing what's missing. Surfaced as a toast at submit time.
    pub fn validate_strategy_context(&self) -> Result<(), &'static str> {
        match self.seed_strategy_idx {
            // Behavioural strategies — always usable.
            0 | 1 => Ok(()),
            // Track requires an explicit id.
            2 => {
                if self.seed_context.track_id.is_some() {
                    Ok(())
                } else {
                    Err("Track strategy needs a seed track — open the form from a Library track row")
                }
            }
            3 => {
                if self
                    .seed_context
                    .artist
                    .as_deref()
                    .map(|s| !s.trim().is_empty())
                    .unwrap_or(false)
                {
                    Ok(())
                } else {
                    Err("Artist strategy needs an artist — open the form from a Library artist or album sidebar row")
                }
            }
            4 => {
                if self
                    .seed_context
                    .genre
                    .as_deref()
                    .map(|s| !s.trim().is_empty())
                    .unwrap_or(false)
                {
                    Ok(())
                } else {
                    Err("Genre strategy needs a genre — open the form from a Library track row whose genre tag is populated")
                }
            }
            _ => Err("Unknown seed strategy"),
        }
    }

    /// Short display of the value backing the currently-selected strategy.
    /// Surfaced next to the radio in the form so the user can see what
    /// they'll generate from. `None` means "no context" — the form
    /// renderer flags this as `(none)`.
    pub fn current_context_label(&self) -> Option<String> {
        match self.seed_strategy_idx {
            0 | 1 => None,
            2 => self.seed_context.track_id.map(|id| format!("track #{id}")),
            3 => self.seed_context.artist.clone(),
            4 => self.seed_context.genre.clone(),
            _ => None,
        }
    }

    /// Form pre-filled to regenerate an existing generated playlist using
    /// its stored params.
    pub fn for_regenerate(playlist: &Playlist) -> Option<Self> {
        let (params, _gen_at) = match &playlist.kind {
            PlaylistKind::Generated {
                params,
                last_generated_ms,
            } => (params.clone(), *last_generated_ms),
            PlaylistKind::Manual => return None,
        };
        let (window_days, seed_count) = match &params.seed_strategy {
            SeedStrategy::TopPlayed { window_days, count } => (*window_days, *count),
            SeedStrategy::RecentlyPlayed { count } => (0, *count),
            _ => (0, 0),
        };
        let mut ctx = SeedContext::default();
        match &params.seed_strategy {
            SeedStrategy::Track(id) => ctx.track_id = Some(*id),
            SeedStrategy::Artist(name) => ctx.artist = Some(name.clone()),
            SeedStrategy::Genre(name) => ctx.genre = Some(name.clone()),
            _ => {}
        }
        Some(GenerationFormState {
            name: playlist.name.clone(),
            seed_strategy_idx: match &params.seed_strategy {
                SeedStrategy::TopPlayed { .. } => 0,
                SeedStrategy::RecentlyPlayed { .. } => 1,
                SeedStrategy::Track(_) => 2,
                SeedStrategy::Artist(_) => 3,
                SeedStrategy::Genre(_) => 4,
            },
            window_days,
            seed_count,
            target_length: params.target_length,
            max_per_artist: params.max_per_artist,
            max_per_album: params.max_per_album,
            diversity: params.diversity_lambda(),
            novelty: params.novelty(),
            exclude_on_device: params.exclude_on_device,
            seed_context: ctx,
            regenerating: Some(playlist.id),
            selected_field: 0,
        })
    }

    /// Compose [`GenerationParams`] from the current field values.
    pub fn to_params(&self) -> GenerationParams {
        let strategy = match self.seed_strategy_idx {
            0 => SeedStrategy::TopPlayed {
                window_days: self.window_days,
                count: self.seed_count.max(1),
            },
            1 => SeedStrategy::RecentlyPlayed {
                count: self.seed_count.max(1),
            },
            2 => SeedStrategy::Track(self.seed_context.track_id.unwrap_or(0)),
            3 => SeedStrategy::Artist(self.seed_context.artist.clone().unwrap_or_default()),
            _ => SeedStrategy::Genre(self.seed_context.genre.clone().unwrap_or_default()),
        };
        GenerationParams {
            seed_strategy: strategy,
            target_length: self.target_length.max(1),
            max_per_artist: self.max_per_artist.max(1),
            max_per_album: self.max_per_album.max(1),
            diversity_lambda_bits: self.diversity.clamp(0.0, 1.0).to_bits(),
            novelty_bits: self.novelty.clamp(0.0, 1.0).to_bits(),
            weights: zytunes::playlist::ScoringWeights::default(),
            exclude_artists: Vec::new(),
            exclude_track_ids: Vec::new(),
            exclude_on_device: self.exclude_on_device,
        }
    }
}

/// Whether the device-side playlist push (Phase 3) is enabled. Off by
/// default after the 2026-04-26 incident: writing a playlist to a real
/// iPod produced a corrupted iTunesDB. Until the write path round-trips
/// safely on hardware, this stays opt-in via
/// `ZYTUNES_EXPERIMENTAL_PLAYLIST_SYNC=1`.
pub fn device_playlist_sync_enabled() -> bool {
    std::env::var("ZYTUNES_EXPERIMENTAL_PLAYLIST_SYNC")
        .ok()
        .as_deref()
        == Some("1")
}

/// Short YYYY-MM-DD-ish date for default playlist names. Cheap stand-alone
/// math beats pulling in `chrono` just for one stamp.
pub(super) fn short_date(now_ms: u64) -> String {
    // Days since unix epoch.
    let total_days = (now_ms / 86_400_000) as i64;
    // Civil-from-days algorithm (Howard Hinnant) — accurate, no leap-year
    // edge cases, no allocations.
    let z = total_days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}
