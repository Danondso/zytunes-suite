# zytunes Recommendation Engine + Playlist Browser — Design Doc

Draft v0. Goal: ship a Discover Weekly-style generator and a playlist
browser TUI screen, grounded in what the codebase already exposes.
Opinionated: each section picks one path and lists alternatives only
where the choice is non-obvious.

---

## 1. Signal inventory

We have more raw signal than typical content-based recommenders because
the lofty pass already populates the wide `Track` bag. Density varies
wildly by source.

### Behavioral signals (the heart of "Discover Weekly")

| Signal | Source | Density | Notes |
|---|---|---|---|
| Aggregate `play_count` | `LocalPlays` (`src/local_plays.rs:69`) | Grows over time; 0 day-1 | TUI plays + per-device delta merge — already authoritative across iPod and Zune (Zune via `apply_playcounts` at `src/mtp/native.rs:1817`, iPod via mhit at `src/mtp/ipod_session.rs:394`). This is the strongest signal we have. |
| Aggregate `skip_count` | `LocalPlays` | Sparse | Negative signal — promote tracks that aren't being skipped. |
| `last_played_at_ms` | `LocalPlays.last_played_at_ms` | Set on real plays only | Drives recency boost / "haven't heard in a while" rotation. Notably **not** fabricated from sync time — that intent matters; respect it. |
| `acoustic_id` | `Track.acoustic_id` (Chromaprint) | Dense for fingerprinted scans | We don't *use* it for recommendations yet (no MB lookup), but having it means the door is open later. v1 ignores it. |
| `rating` (POPM byte) | `Track.rating` from file tag | Sparse — most users don't rate | Treat as a strong explicit boost when present (`Some(>= ~80)`), do not penalize absence. |

### Content signals (intrinsic to the track)

| Signal | Field | Density | Useful for |
|---|---|---|---|
| Genre | `Track.genre` | Medium-high | Primary similarity dimension. Tag noise (`"Rock; Indie"`, `"rock"` vs `"Rock"`) needs cheap normalization. |
| Year | `Track.year` | High | Era cohesion — recs from "your 2008 plays" feel right. |
| Artist | `Track.artist` (always-present) | 100% | Both a similarity dimension and the strongest place to enforce diversity (cap N tracks per artist per playlist). |
| Album / album_artist | `Track.album`, `album_artist` | High | Diversity caps. |
| BPM | `Track.bpm` | Sparse — DJ libraries only | Skip in v1 unless population test shows >40% coverage. |
| Initial key | `Track.initial_key` | Very sparse | Skip in v1. |
| Mood | `Track.mood` | Very sparse | Skip in v1. |
| Composer / conductor | `Track.composer`, `conductor` | Classical-only | Skip in v1, future "classical recs" angle. |
| Duration | `Track.total_time_ms` | High | Soft filter — exclude <30 s skits and >12 min (album-side longforms). |
| MusicBrainz IDs | `mb_*` | If user runs Picard | Phase 2+ collaborative-style "people who tagged this also tagged…" via local data only — but we don't have any external corpus, so it's just an exact-identity hash. Skip v1. |
| ReplayGain track gain | `replaygain_track_gain` | Medium for ripped libs | Audio-loudness similarity proxy — interesting but low ROI. Skip v1. |

### What we structurally don't have

- **Collaborative filtering**: one user, no peer matrix. Off the table forever for offline mode.
- **Learned audio embeddings**: no model, no GPU pipeline. Off the table.
- **Co-listen sequences**: we count plays, but we don't log *order* or *session boundaries*. Cheap to add later (append-only line `(unix_ms, track_id)` log) — flag for Phase 4.
- **Lyric / NLP signal**: lyrics field exists but unused. Skip — too domain-heavy.
- **External enrichment** (Last.fm tags, Discogs, MB taxonomy): excluded by the offline constraint.

### Practical floor

Behavioral (`play_count`, `last_played`, `skip_count`) + genre + year +
artist diversity is the floor — and it's enough. Hand-tuned this gives
results that feel surprisingly close to "Daily Mix" (genre-cohesive,
familiar-but-not-most-played, era-coherent). The fancier signals
(BPM/key/mood) are too sparse in real libraries to carry weight.

---

## 2. Recommendation algorithm

### Recommended for v1: hand-tuned weighted scoring with seed expansion

**Why:** explainable end-to-end (we can show "why this track" in the
popup), tunable without retraining, ships in a week. The literature is
clear that for single-user offline content-based recs, a well-tuned
scorer beats a poorly-tuned ANN every time, and we have no labeled data
to fit a model against anyway.

**Shape of the algorithm:**

```text
generate(library, plays, params) -> Vec<TrackId>:
  seeds = pick_seeds(plays, params.seed_strategy, params.seed_count)
  candidates = library.all_tracks()
                      .filter(|t| eligible(t, params))   // duration, format, etc.
  scored = candidates.map(|t| (t, score(t, seeds, plays, params)))
                     .filter(|(_, s)| s > params.threshold)
  selected = mmr_select(scored, params.target_len, params.diversity_lambda)
              // Maximal Marginal Relevance: at each pick step,
              // argmax over (score - λ · max_similarity_to_already_picked)
              // Solves "always recommends the same 5 tracks" by penalizing
              // proximity to already-picked items in the same generation.
  apply_artist_cap(selected, params.max_per_artist)  // hard diversity bound
```

**`score(t, seeds, plays, params)` is a weighted sum of normalized terms:**

```text
score = w_genre   * genre_overlap(t, seeds)            // 0..1, Jaccard on normalized genre bag
      + w_year    * year_proximity(t, seeds)            // 0..1, exp decay over |year - mean(seed years)|
      + w_artist  * artist_affinity(t, plays)           // 0..1, log-scaled artist play-count (NOT t in seeds)
      + w_recency * (1 - recency_score(t, plays))       // 0..1, boost tracks NOT played recently
      + w_rating  * rating_boost(t)                     // 0..1, 0 unless POPM>=80
      - w_skip    * skip_penalty(t, plays)              // 0..1, log-scaled skip count
      + novelty_jitter()                                // small noise, breaks ties, drives weekly rotation
```

Default weights (tunable via `params`): `w_genre=0.30, w_year=0.10,
w_artist=0.30, w_recency=0.20, w_rating=0.05, w_skip=0.20`. These are
starting points — expose them via the generation form and via
`config.toml` so the user can tune to taste.

**Seed strategies** (user-selectable):

- `TopPlayed`: top-K by `play_count` from the last 30 days (or all-time
  if no recent plays). The "Discover Weekly" default.
- `RecentlyPlayed`: most-recent K tracks by `last_played_at_ms`.
- `Track(id)`: explicit single seed → "more like this." (Used by an
  inline TUI action: `R` on a track row → "Generate similar.")
- `Artist(name)` / `Genre(name)`: scoped exploration.

**How the weekly rotation feels fresh:**

1. Seed-set rotation: weekly regen draws a random sample of seeds
   instead of always the top-K (`pick_seeds` with reservoir sampling
   weighted by play count).
2. `novelty_jitter()` adds bounded noise (~0.02) so equal-scored
   candidates shuffle.
3. Seed the RNG with `(week_number, library_root_hash)` so two regens
   in the same week are deterministic but week-over-week differs.
4. Track-level "exclude recently recommended" memory: the playlist
   record stores `Vec<TrackId>` of items it generated, and successive
   regens of the same playlist soft-penalize the previous picks
   (configurable, default ON).

**Anti-degeneracy guards** (the actual hard problems):

- Hard cap: max 2 tracks per artist per playlist (`max_per_artist`).
- Hard cap: max 1 track per album.
- Soft cap on genre: top genre ≤ 50% of playlist (back off via MMR
  diversity term).
- Excluded-from-recs flag in the playlist's generation_params (per-user
  blocklist of artists or tracks).
- "Already on device" filter: optional, default ON when generating
  while connected — bias toward unfamiliar without re-recommending
  what's already synced.

### Alternatives considered

**Nearest-neighbor in feature vector space.** Build a per-track vector
`[onehot(genre), normalize(year), normalize(log(play_count)),
normalize(bpm), …]`, query by cosine distance to the seed vector. More
"correct" but: (1) requires careful normalization of every dimension,
(2) one-hot genre vectors blow up wide for libraries with many tags,
(3) doesn't naturally express the asymmetric "I want similar but not
already-played-to-death" goal — you bolt that on as a re-ranking step,
which is exactly what the weighted scorer does directly. **Verdict:**
strictly worse for this size of problem.

**Markov-chain / co-listen graph.** Genuinely strong for sequence
recommendation (next-track-in-queue), but we don't currently log
ordered listen sessions. We *should* start logging — append-only line
log of `(unix_ms, track_id, played_through?)` is ~30 bytes/event,
trivially cheap. **Verdict:** Phase 4 enhancement, not v1. Stub the
log file in Phase 1 to start collecting data we'll need later.

**Other candidates considered and rejected for v1:**

- TF-IDF over genre/tag bags: fine but diminishing returns over Jaccard
  given how short our tag lists are.
- Item-based CF using only this user's plays as the matrix: degenerates
  to "tracks frequently played in proximity," which is the Markov path
  in disguise.

---

## 3. Playlist data model & persistence

### The `Playlist` struct

```rust
// new file: src/playlist.rs
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Playlist {
    pub id: u64,                         // stable, hashed from (created_at_ms, name)
    pub name: String,
    pub kind: PlaylistKind,              // Manual | Generated { params }
    pub track_ids: Vec<u64>,             // library Track.id, ordered
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    /// IDs of tracks generated by previous runs of this playlist's
    /// `Generate*` command — used as soft-penalty during regeneration so
    /// the "same playlist, regenerate" action drifts instead of repeats.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub previously_recommended: Vec<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum PlaylistKind {
    Manual,
    Generated { params: GenerationParams, last_generated_ms: u64 },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GenerationParams {
    pub seed_strategy: SeedStrategy,
    pub target_length: usize,
    pub max_per_artist: usize,
    pub max_per_album: usize,
    pub diversity_lambda: f32,    // MMR diversity weight
    pub novelty: f32,             // 0..1, scales novelty_jitter + previously_recommended penalty
    pub weights: ScoringWeights,
    pub exclude_artists: Vec<String>,
    pub exclude_track_ids: Vec<u64>,
    pub exclude_on_device: bool,  // when device connected, omit already-synced tracks
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum SeedStrategy {
    TopPlayed { window_days: u32, count: usize },
    RecentlyPlayed { count: usize },
    Track(u64),
    Artist(String),
    Genre(String),
}
```

Manual and generated unify into one type, distinguished by `kind`. Same
view, same key bindings, same sync flow — just different create/refresh
code paths. Two enum variants beats two separate types because every
operation downstream (rename, delete, sync, browse, reorder) is
identical.

### Persistence: `~/.config/zytunes/playlists.json`

**Pick:** `~/.config/zytunes/`, sibling to `config.toml`, **not**
`~/.cache/`. Reasoning:

- Playlists are **user-authored state**, not a regenerable cache.
  Same category as `config.toml`. If `~/.cache` gets blown away by a
  cache-cleaner cron or `rm -rf ~/.cache/*` (a thing people do),
  losing playlists would be unacceptable.
- `local_plays.json` lives in `~/.cache` because it is technically
  rebuildable by re-merging device counters; playlists are not.
- File location is a single function in a new `src/playlist_store.rs`,
  mirroring the layout of `local_plays.rs`.

Schema versioning copies the `cache.rs` pattern verbatim:

```rust
const PLAYLIST_SCHEMA_VERSION: u32 = 1;

#[derive(Serialize, Deserialize, Default)]
struct OnDisk {
    #[serde(default)]
    schema_version: u32,
    #[serde(default)]
    playlists: Vec<Playlist>,
}
```

On schema mismatch, **rename** the existing file to
`playlists.json.v{N}.bak` rather than discarding silently — playlists
are too valuable to drop without a recovery path. (Distinct from
`local_plays`/dirlib cache, which are regenerable.)

Atomic write via `.tmp` + rename, same pattern as
`LocalPlays::save_to` (`src/local_plays.rs:226`).

### No new trait abstraction

Don't parallel `MusicLibrary`. There's only one playlist store
(library-side, single user, single file), so a trait is overhead. A
struct on `App` (`pub playlists: PlaylistStore`) holding the
`Vec<Playlist>` plus IO methods is the right shape. The recommender
itself **does** want a trait-shaped contract because we'll want to
swap implementations in tests:

```rust
pub trait Recommender {
    fn generate(&self, lib: &dyn MusicLibrary, plays: &LocalPlays,
                params: &GenerationParams) -> Vec<u64>;
}
pub struct WeightedRecommender;  // the v1 implementation
```

### Sync queue interaction

Playlists become a first-class queue source. The existing
`SidebarEntry` enum (`src/tui/app.rs:54`) gets a third variant so
"add playlist to sync queue" routes through the same machinery as
"add artist" or "add album":

```rust
pub enum SidebarEntry {
    Artist(String),
    Album { artist: String, album: String },
    Playlist { id: u64, name: String },   // new
}
```

Pressing `a` on a playlist enqueues all its constituent tracks via
`add_sidebar_item_to_queue` (`src/tui/app.rs:3095`) — no new sync
plumbing, the queue is already track-id-keyed.

### Device-side playlist sync (Phase 3)

**iPod**: `ipod-db` already supports playlists end-to-end —
`IpodPlaylist` (`ipod-db/src/lib.rs:222`), `mhyp/mhip` writers
(`itunesdb_write.rs`), master-playlist invariant. Wiring a zytunes
`Playlist` → `IpodPlaylist` is roughly:

1. Map `Playlist.track_ids` (library `Track.id`) → iPod `dbid` by
   matching `(artist, album, title)` against `IpodDatabase.tracks`.
2. Skip tracks not yet on the device, OR push them first if
   `auto_push_on_sync` is true (recommended default).
3. Append/replace the corresponding `IpodPlaylist` in
   `db.playlists`, flush via existing `flush()`
   (`src/mtp/ipod_session.rs:38`).

**Zune**: native MTP playlist support via the playlist object format
(`0xBA05` ABSTRACT_AUDIO_VIDEO_PLAYLIST). Untested in this codebase —
mark as Phase 3b research spike, after iPod is shipping.

For v1 of the engine + UI we're explicitly not touching this. The
playlist lives library-side only; "sync this playlist" pushes its
**tracks**, not the playlist record itself.

---

## 4. TUI design

### Top-level integration: third `BrowseMode`

`BrowseMode::Playlists` joins `Library` and `Device`
(`src/tui/app.rs:44`). Toggle key `v` cycles
`Library → Device → Playlists → Library`. When a device isn't
connected, `Device` is skipped (existing pattern at
`src/tui/app.rs:2972`).

Rationale over "third sidebar mode under Library": playlists need
their own sidebar (the playlist *list*), their own track-area
behavior (selected playlist's tracks, not artist/album-derived
tracks), and their own footer hints. Cramming into Library would
overload the `1`/`2` keys and force the renderer to branch deeply at
every panel boundary. A new top-level mode is the cleaner cut and
matches how `Device` was added.

### Layout in Playlists mode

Reuses the existing three-column layout (`src/tui/ui.rs`) with
panel meanings reassigned:

- **Sidebar (left of center column)**: list of playlists, sorted with
  Generated above Manual, then by `updated_at_ms` desc. Visual
  distinction:
  - Manual: plain name.
  - Generated: small `~` prefix + dim subtitle showing
    `seed_strategy.short_name() + " • last gen " + relative_time`.
  - Empty state: "No playlists yet. Press `N` to create one or `G` to
    generate Discover Weekly." centered in the panel.
- **Albums panel**: hidden in this mode (the layout already
  conditionally hides it via `has_album_browser()` —
  `src/tui/app.rs:3039`; extend that predicate).
- **Track list panel**: tracks in the selected playlist, in playlist
  order. Same row renderer as Library mode — no new code needed.
- **Footer**: context-sensitive hints for the new bindings.
- Left column (device + sync queue + log) unchanged.

### Key bindings

Designed to extend, not collide with, the existing scheme. Already
taken: `1 2 4 a A C D h I L P r S t T U v X / ?`, plus all arrows,
`Enter`, `Tab`, `Space`. Available among letters: `b e f g i j k m o
w x y z B E F G H J K M N O Q R V W Y Z`.

- `3` — switch to Playlists sidebar mode (parallels `1` Artists, `2`
  Albums; reuses the digit-key family).
- `N` — new manual playlist (open name-input modal, same modal style
  as search).
- `G` — open the **Generation form** (modal). Pre-fills with last-used
  params or sensible defaults; `Enter` runs generation, result becomes
  a new generated playlist named e.g. `Discover Weekly · Apr 26`.
- `R` — regenerate the selected generated playlist (re-runs with the
  stored `GenerationParams`, soft-penalizing
  `previously_recommended`). On a Library-mode track row, `R` is also
  the "more like this" shortcut → opens the form pre-filled with
  `SeedStrategy::Track(id)`.
- `e` — rename the selected playlist (open input modal pre-filled).
- `d` on a playlist sidebar row — delete playlist (confirmation overlay,
  same pattern as the existing removal-confirmation overlay used in
  Device mode).
- `a` on a playlist sidebar row — enqueue all its tracks for sync (the
  unified flow described above).
- `?` — help overlay grows a Playlists section automatically.

`Tab` cycling order in Playlists mode: Library (sidebar) → TrackList →
Device → SyncQueue → Library. Same as today minus the Albums panel.

### The Generation form modal

Use the existing overlay pattern (`draw_track_info_overlay`,
`src/tui/ui.rs:2635`): centered popup, percentage-of-terminal sizing
clamped to a reasonable min/max, dim background. Fields:

```
┌─ Generate Playlist ─────────────────────────────────────┐
│  Name           [Discover Weekly · Apr 26 ▏]            │
│  Seed strategy  ( ) Top played   (•) Recent             │
│                 ( ) This track   ( ) This artist         │
│                 ( ) This genre                          │
│  Window         [30] days    Seed count [10]            │
│  Length         [25] tracks                             │
│  Max per artist [2]                                     │
│  Diversity      [============-----] 0.7                 │
│  Novelty        [=========--------] 0.5                 │
│  [x] Exclude tracks already on connected device         │
│  [x] Exclude tracks I've skipped 3+ times                │
│                                                         │
│   Tab/Shift-Tab move • Space toggle • Enter generate    │
└─────────────────────────────────────────────────────────┘
```

Implementation note: build out a small `GenerationFormState` on
`App`, mirroring how `theme_picker_state` is structured. Keep it
self-contained so it can grow form fields without rippling.

### Visual distinction: generated vs manual

In the playlist sidebar:

- Manual: `playlist_name`
- Generated: `~ playlist_name` plus a dim second line
  `└ recent · 25 tracks · 2d ago`

The leading `~` (configurable per theme) is borrowed from the
"generated/synthetic" convention and is one ASCII char so it works
even with the existing CJK width caveat. No new color allocation —
reuse `theme.dim_text` for the subtitle.

In the track list, generated playlists get a small footer hint:
`generated · seed: top-10 last 30d · regen: R · params: G`. The full
`GenerationParams` are available in the track-info popup body via a
new "Playlist context" section when the popup is opened from a
generated playlist.

### Empty / failure states

- No playlists at all: empty state in sidebar with two CTAs (above).
- Generated playlist with zero results: keep the playlist record but
  the track list shows "No tracks matched these parameters. Press G
  to relax constraints." Don't auto-delete — the user picked these
  params for a reason; leave it visible.
- Generation while library is still scanning: toast "Library still
  loading; try again in a moment" and abort. Don't block.

### Toast and confirmation reuse

All destructive paths (delete playlist, overwrite-on-regenerate)
route through the existing toast + confirmation overlay machinery
seen in `pending_removal` (`src/tui/app.rs:3114`). No new modal
infrastructure.

---

## 5. Phased implementation plan

### Phase 1 — Playlist data model + manual playlists in TUI (~3-5 days)

Goal: ship a working playlist browser with manual create / rename /
delete / track add. No generator.

Files to touch:

- **new** `src/playlist.rs` — `Playlist`, `PlaylistKind`,
  `GenerationParams`, `SeedStrategy`, `ScoringWeights`. (Defining the
  generator types now even though Phase 1 doesn't use them keeps the
  on-disk schema stable across phases.)
- **new** `src/playlist_store.rs` — `PlaylistStore` with `load`,
  `save`, `add`, `remove`, `rename`, `add_track`, `remove_track`,
  `move_track`. Mirrors `local_plays.rs` style: `OnDisk` struct,
  schema version, atomic write, capturing-logger tests. **Backup on
  schema-mismatch** instead of discard.
- `src/lib.rs` — `pub mod playlist; pub mod playlist_store;`.
- `src/tui/app.rs` —
  - `BrowseMode::Playlists` variant + `toggle_browse_mode` rotation.
  - `App::playlists: PlaylistStore` field.
  - `SidebarEntry::Playlist { id, name }` variant + `display`,
    `nav_key`, `lowercase_key` arms.
  - Sidebar/trackList build paths for Playlists mode.
  - New key bindings: `3`, `N`, `e`, `d` on playlist row.
  - "Add to sync queue from playlist" via existing
    `add_sidebar_item_to_queue`.
- `src/tui/ui.rs` — sidebar/footer rendering for Playlists mode;
  `has_album_browser()` returns `false` in Playlists mode; help
  overlay text additions.
- Tests: Playlist round-trip, schema-bump backup behavior, sync queue
  enqueues all playlist tracks, sidebar entry display strings.

Demo: create "My Faves," add tracks via `a` from track list, push to
device.

### Phase 2 — Recommendation engine + generated playlist UI (~5-7 days)

Goal: ship `Recommender` + Generation form + regenerate flow.

Files to touch:

- **new** `src/recommender.rs` — `Recommender` trait,
  `WeightedRecommender`. Pure functions for `score`, `pick_seeds`,
  `mmr_select`, `apply_artist_cap`, `eligible`. Heavy unit tests of
  scoring math against synthetic libraries — this is the part that
  rots silently if untested.
- **new** `src/genre_norm.rs` — cheap genre normalization (lowercase,
  strip parenthesized counts, split on `;` / `/`, dedupe). 50 LOC.
- `src/playlist.rs` — `GenerationParams::default_discover_weekly()`
  factory.
- `src/tui/app.rs` —
  - `GenerationFormState` + `show_generation_form` flag.
  - Key bindings: `G` open form, `R` regenerate, `R` on a track row in
    Library mode → "more like this."
  - `App::run_generation(params) -> Result<u64, String>` that
    consults `library`, `local_plays`, optionally `device.track_set`,
    creates the resulting `Playlist`, returns its id.
- `src/tui/ui.rs` —
  - `draw_generation_form_overlay`.
  - Generated-playlist visual distinction (sidebar prefix, subtitle).
  - Track-info popup learns to show "Why this track?" section when
    invoked from inside a generated playlist (uses scoring breakdown
    returned from `WeightedRecommender::generate_with_explanations`).
- `src/tui/config.rs` — optional `[recommender]` table for default
  weight overrides; document in CLAUDE.md.
- Tests: the scoring math, MMR behavior on a degenerate "all the same
  artist" candidate set, novelty rotation seed determinism within a
  week and divergence across weeks.

Demo: `G` → defaults → `Enter` → 25-track playlist that feels right.
`R` regenerates with drift.

### Phase 3 — Device-side playlist sync (~3-5 days for iPod, +TBD for Zune)

Files to touch:

- `src/mtp/mod.rs` — extend `DeviceSession` with
  `import_playlist(name: &str, ordered_track_ids: &[u64]) -> Result<(), String>`
  where `track_ids` are *device* IDs (iPod dbid / Zune object handle),
  not library IDs.
- `src/mtp/ipod_session.rs` — implementation: build/replace
  `IpodPlaylist` in `db.playlists`, flush. Already-resolvable from
  `db.find_track_by_*` helpers.
- `src/mtp/native.rs` — Zune `import_playlist` is the spike;
  references the AAVT format from libmtp + experimental probes.
- `src/tui/app.rs` — `S` on a generated/manual playlist while a device
  is connected pushes both the constituent tracks (existing path) AND
  the playlist record (new). Library→device track-id resolution
  happens here, not in the session.
- Tests: round-trip of an iPod database with a fresh playlist;
  verify master playlist still contains all tracks; verify
  `IpodPlaylist::track_ids` order matches input.

Defer Zune until iPod is solid; spike-and-stop if Zune playlist
import doesn't work in 2 days of probing.

### Phase 4 (later) — Co-listen logging + sequence-aware mode

Stub the append-only listen log in Phase 1 (`local_plays` adjacent),
collect data passively, ship the Markov-chain "play next" sidecar
recommender once we have a few months of data.

---

## 6. Open questions / risks

1. **Auto-refresh cadence.** Should "Discover Weekly" regenerate
   itself weekly (cron-like, on TUI launch if >7 days since
   `last_generated_ms`), or strictly on-demand via `R`? **Default
   choice unless told otherwise:** on-demand, with a soft prompt at
   launch ("Your Discover Weekly is 9 days old — press `R` to
   refresh"). Auto-replacement of user-facing state without a
   keystroke feels wrong for a music-nerd tool.

2. **Per-generation vs global novelty/weights.** Pick: store on each
   `GenerationParams` (per-playlist), allow `[recommender]` defaults
   in `config.toml` for new generations. Don't introduce a global
   "novelty slider" UI — the form covers it.

3. **Exclude already-on-device by default?** Pick: yes when device is
   connected at generation time. The whole point of the workflow is
   "what should I add next," and recommending what's already loaded
   defeats it. Toggle in the form for the rare reverse case.

4. **Library track ID stability.** `Track.id` is `hash_path(path)`
   (`src/dirlib.rs:363`). Renaming/moving a file changes its ID,
   which would silently break a playlist's `track_ids`. Mitigations,
   in order of preference:
   - **(picked)** Resolve playlists at load time: any unresolvable
     `track_id` is replaced by an `(artist, album, title)` lookup
     (using `tracks_by_name` plus a filter), with a stash of the
     last-known triple persisted alongside each ID. Drop entries
     that don't resolve, log a warning.
   - Switch to `acoustic_id` as the canonical playlist track key —
     more stable but sparse on first scan and `None` for files
     fingerprinting can't decode.
   - Punt: document that moving files breaks playlists.

5. **Empty libraries / brand-new users.** Day-1 the user has
   `play_count = 0` everywhere — `TopPlayed` produces nothing.
   Fallback: when no behavioral signal exists, the recommender
   degrades to "diverse sample of the library by genre/artist/year"
   (essentially a stratified shuffle). Document this explicitly
   so the user understands it's not broken.

6. **Tag-noise normalization.** Real libraries have `"Rock"`,
   `"rock"`, `"Rock; Indie"`, `"(17)Rock"`. Genre overlap math goes
   bad fast without normalization. Building this right is a half-day
   on its own — `src/genre_norm.rs` is a real module, not a one-liner.

7. **Recommender-only deps.** Want anything new from crates.io? The
   v1 plan needs nothing — pure Rust + `serde` + existing deps.
   Avoid adding anything; we already pay heavy compile time.

8. **Diagnostic dump.** Should `WeightedRecommender::generate` accept
   a `Logger` (parallel to `cache.rs`'s pattern) so the TUI can pipe
   "considered N candidates, picked K" into the sync log panel?
   Recommend: yes, low cost, useful for tuning weights.

9. **Cancel-during-generation.** With a ~10k-track library the score
   pass is well under a second, but with extended scoring +
   per-candidate explanations it could approach noticeable. Run
   generation on the existing background worker
   (`tui/background.rs`), not inline on the UI thread. Existing
   `BgCommand` / `BgEvent` infrastructure handles this directly —
   add `BgCommand::Generate(GenerationParams, name)` and
   `BgEvent::GenerationComplete(playlist_id)`.

---

## 7. Implementation status (as-of 2026-04-26)

All four phases shipped. **706 tests passing** workspace-wide, clippy
`-D warnings` clean, fmt clean, release builds green.

### Phase 1 — playlist data model + manual playlists in TUI ✅

Shipped as designed.

- `src/playlist.rs` carries `Playlist`, `PlaylistKind`,
  `GenerationParams`, `SeedStrategy`, `ScoringWeights` (all five strategy
  variants defined).
- `src/playlist_store.rs` does load/save/add/remove/rename/track ops with
  schema-versioned JSON. **Schema mismatch backs up to
  `playlists.json.v{N}.bak`** rather than discards (the deliberate
  divergence from `LocalPlays` and `cache.rs`).
- `BrowseMode::Playlists` is a third top-level mode reachable via `v`
  (cycle is `Library → Device → Playlists` with Device skipped when
  nothing's connected).
- `SidebarEntry::Playlist { id, name }` round-trips through the existing
  sidebar pipeline. Manual / generated playlists distinguish via a `~ `
  prefix in the sidebar.
- Key bindings: `N` new, `e` rename, `d` delete (with confirmation), `a`
  enqueue all tracks for sync, `+` add a Library track to a playlist via
  picker.
- Modal helpers (name input, delete confirm, picker) all routed through
  the new `Theme::modal()` / `modal_dim()` styles so contrast holds on
  every preset (regression caught on Newport Lights).

### Phase 2 — recommendation engine + Generation form ✅

Shipped with all five seed strategies wired end-to-end.

- `src/genre_norm.rs` — strip ID3v1 `(NN)` prefix, split on `;`/`/`,
  lowercase, dedupe, plus Jaccard.
- `src/recommender.rs` — `Recommender` trait + `WeightedRecommender`. Pure
  fns: `pick_seeds`, `eligible`, `score`, `mmr_select`,
  `apply_artist_cap`, plus `SeedContext` cache.
- `WeightedRecommender::generate` takes the bigram table from Phase 4 as
  `Option<&BigramTable>`; existing tests pass `None`.
- Hard caps: max 2 per artist (default), max 1 per album, exclude seeds,
  exclude blocklist artists / track IDs, optional exclude-already-on-device.
- **Generation form modal** with 10 fields: name, seed-strategy radio
  (5 options), window/seed-count/length/per-artist/per-album numerics,
  diversity & novelty sliders, exclude-on-device checkbox.
- **All five seed strategies are reachable from the form**:
  - **TopPlayed / RecentlyPlayed** — work without context.
  - **Track** — `G` from a Library track row pre-fills the track id.
  - **Artist** — `G` from a Library Artist sidebar row, an Album sidebar
    row, or the Albums panel pre-fills the artist name.
  - **Genre** — `G` from a Library track row also captures the track's
    genre tag; the user toggles the radio in the form to use it.
  - The strategy row in the form shows the backing context next to the
    radio (`( By artist )  Boards of Canada`) or `(none)` when the
    selected strategy lacks data; submit rejects with a clear toast.
- `ScoringWeights` exposes `w_genre / w_year / w_artist / w_recency /
  w_rating / w_skip / w_sequence` (sequence added in Phase 4) with
  per-playlist override storage.

**Not yet shipped** (deferred from the original Phase 2 plan):

- "Why this track?" scoring breakdown in the track-info popup. The
  underlying `score` math is exposed and tested but the UI hookup
  (`generate_with_explanations`) doesn't exist.
- `[recommender]` config table for global default-weight overrides.
- Generation runs **inline**, not on the background worker. ~10k-track
  libraries score in well under a second so this hasn't bitten anyone;
  if extended scoring + explanations land it should move to bg.

### Phase 3 — device-side playlist sync ⚠️ (iPod gated off after 2026-04-26 incident) / 🔬 (Zune)

**Status: regressed.** iPod side is wired but **disabled by default**.

**2026-04-26 incident.** The first real-hardware playlist write on a
user's iPod produced a corrupted iTunesDB — music vanished from the
device UI after writing a single playlist. Recovery: the
`itunesdb_write::write_to_disk` path produces a `.bak` sibling
(`iPod_Control/iTunes/iTunesDB.bak`) before each write; copying that
back over `iTunesDB` restored the user's library. Root cause is not
yet identified.

Hypotheses worth investigating, ranked by suspicion:

1. **`reassign_track_ids` interacts badly with playlist-only writes.**
   `IpodSession::flush` reassigns every track's `track_id` from 52
   sequentially on every flush, then patches the raw mhit blob's
   `track_id` field at +0x10. Playlist mhips reference tracks via
   `dbid_to_track_id`, which is rebuilt fresh during serialise — so
   the lookup *should* see the new IDs. But if the master playlist's
   `track_ids` (parsed in as dbids) doesn't survive the parser
   round-trip — e.g. if the parser left them empty or mis-mapped —
   then the rewritten master playlist would be empty even though the
   tracks themselves are intact, which matches "music vanished from
   the UI."
2. **Persistent IDs reassigned by index.** `serialize` writes each
   playlist's persistent ID as `(i + 1) as u64`. iTunes uses 64-bit
   randoms; the firmware may not care, but the master playlist's
   persistent ID changes every write, which iTunes-style smart
   playlists / ratings sidecars might depend on.
3. **Smart playlists.** The parser preserves the smart-playlist
   blob (`raw_smart_playlists`) and the writer replays it verbatim.
   Adding a new playlist between the master and any preserved smart
   playlist could shift offsets the firmware relies on.

**Mitigation shipped with the incident response (commit on this
branch):**

- `App::experimental_playlist_sync: bool`, default `false` from the
  env var `ZYTUNES_EXPERIMENTAL_PLAYLIST_SYNC=1`.
- When the flag is off, enqueueing a playlist for sync still pushes
  the file uploads but **does not** queue an `ImportPlaylist` BgCommand
  — only the device-side playlist record is skipped, library-side
  playlists are unaffected.
- The sync log surfaces a clear line explaining what was skipped and
  how to opt back in.
- Test coverage: `enqueue_playlist_with_sync_off_skips_pending_import`
  pins the default-off behaviour; `enqueue_playlist_registers_pending_import`
  flips the flag explicitly to verify the opt-in path.

Existing surface still in place (the trait, the iPod impl, the bg
plumbing) so the fix can be a one-flip-of-the-default once root cause
is found. **Next step**: reproduce on a test iPod with the flag on,
diff the iTunesDB before / after to find what `serialize` mangled.

iPod side wiring (still present, gated):

- `DeviceSession::import_playlist(name, &[(artist, album, title)])`
  with default `Err("not supported")` so backends opt in.
- `PlaylistImportSummary { resolved, skipped, replaced }` so the toast
  surfaces concrete counts.
- **iPod**: `upsert_playlist` resolves tuples to dbid case-insensitively,
  preserves the master-playlist invariant at index 0, replaces by name,
  rejects empty / Library / Master aliases. Pure DB mutation so unit
  tests don't need a writable mount; `IpodSession::import_playlist`
  wraps it with the existing hash58-signing flush.
- **Zune**: stub returning `Err` with a comment outlining the AAVT
  (`0xBA05`) + `SetObjectReferences (0x9810)` flow that needs hardware
  probing. Marked Phase 3b in the source.
- TUI orchestration: `App.pending_playlist_imports` populates when a
  playlist is enqueued via `a`; on `BgEvent::SyncComplete` they drain
  into `BgCommand::ImportPlaylist` so the device-side resolver sees the
  freshly-uploaded tracks. `clear_queue` and Esc-during-sync both drop
  pending imports.

### Phase 4 — listen log + sequence-aware recommender ✅

Shipped earlier than the doc anticipated (the doc said "ship once we
have a few months of data" — collecting *and* applying lit up together
so the recommender starts contributing the moment the threshold clears).

- `src/listen_log.rs` — append-only JSONL at
  `~/.cache/zytunes/listen-log.jsonl`. Each TUI play / skip appends one
  event. Sessions are split on a 30-min gap (configurable). `O_APPEND`
  writes so concurrent worktrees can't corrupt the file.
- `BigramTable::from_log` builds `(prev_id, next_id) → weight` counts
  within sessions (self-loops dropped, skips half-weight). Scoring adds
  `w_sequence * max P(candidate | seed)` as a re-ranking term.
- `BigramTable::is_useful()` gates contribution on ≥20 events / ≥2
  sessions so brand-new users aren't biased by spurious one-offs.
- `commit_generation_form` builds a fresh `BigramTable` from the log on
  every generation and passes it through.

**Not yet shipped** (capture is wired, visibility isn't):

- No UI surface tells the user the sequence model exists or whether it
  contributed to the last generation. A status line in the form
  (`Sequence model: 47 events / 5 sessions (active)` or
  `Sequence model: 12 events (need 20+, 2+ sessions)`) and a tag in the
  post-generation toast (`(seq model: on)`) would close that gap with
  ~50 LOC.

## 8. Outstanding work

In rough priority order of "value vs cost":

1. **Sequence-model visibility in the Generation form** — small,
   immediately useful. Phase 4 is invisible without it.
2. **"Why this track?" breakdown** in the track-info popup when invoked
   from a generated playlist. Requires
   `WeightedRecommender::generate_with_explanations` returning per-track
   term contributions; the popup body already supports a new section.
3. **Background worker for generation** when explanations land. Existing
   `BgCommand` / `BgEvent` infrastructure makes this 1-2 hours.
4. **`[recommender]` config table** for default-weight overrides — only
   matters for users tuning beyond the defaults.
5. **Track-id stability mitigation** (the design doc's open question
   #4). Right now moving a file silently breaks any playlist that
   referenced it. The picked mitigation (load-time resolve via
   `(artist, album, title)` triples) needs persisting the triple
   alongside each track id at save time.
6. **Auto-refresh prompt** at TUI launch when the most-recent generated
   playlist is more than a week old (open question #1's picked default).
7. **Zune playlist import (Phase 3b)** — hardware-dependent research
   spike. Stubbed with notes pointing at the libmtp AAVT flow.
8. **Append-only co-listen log → richer sequence models**. Bigram
   captures pairwise; trigrams or HMM-style next-track prediction
   become possible once the log has months of data. Out-of-scope for v1
   but the data is now collecting.

---

## 9. Open questions / risks (original design-phase notes — mostly resolved)
