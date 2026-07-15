# Stem-Split Playback Plan

Goal: while a track is playing in the TUI, show a per-stem strip in the
now-playing panel — **Vocals / Drums / Bass / Guitar / Piano / Other**
(6-stem `htdemucs_6s`) — and let the user toggle each stem on and off live,
karaoke-style, without stopping playback.

```
┌ Now Playing ──────────────────────────────────────────────────────────┐
│  ▶ Massive Attack — Teardrop                              2:41 / 4:33  │
│  ████████████████████░░░░░░░░░░░░░                                     │
│  STEMS [1 Voc ●] [2 Drm ●] [3 Bas ○] [4 Gtr ●] [5 Pno ●] [6 Oth ●]     │
└────────────────────────────────────────────────────────────────────────┘
```

Filled dot = audible (theme `success` colour), hollow dot = muted (theme
`dim`). The full strip needs ~66 columns of panel width; below that the
labels drop: `STEMS [1●][2●][3○][4●][5●][6●]` (~36 columns).
While separation runs the strip shows `STEMS  separating… 43%`; while the
engine is being provisioned it shows `STEMS  installing engine… (one-time)`.

Decisions locked in with the user:

- **Backend: Python `demucs` in a subprocess** (not in-process ONNX). Cheaper
  by an order of magnitude to build, crash-isolated from the TUI, and the
  `StemSeparator` trait keeps an in-process Rust backend possible later.
- **6 stems** via the `htdemucs_6s` model (vocals, drums, bass, guitar,
  piano, other). Known caveat: the piano stem is the weakest of the six.
- **Stem cache cap: 10 GB** (LRU).
- **No install during first-time setup.** The Python engine is provisioned
  lazily, on the first `M`-press, in the background — never during startup
  or library scan.
- **Provisioning is configurable from `config.toml`** (see §2).

---

## 1. Separation strategy

Real-time source separation inside the TUI process is not feasible — the
usable models run at or below real time on CPU and need a multi-hundred-MB
runtime. So separation is an **offline, cached, background step**, like the
existing transcode/rip pipelines, executed by shelling out to the demucs
CLI:

```
demucs -n htdemucs_6s --flac -o <tmpdir> <input>
```

The separator sits behind a trait so the engine stays swappable:

```rust
// src/stems.rs
pub enum StemKind { Vocals, Drums, Bass, Guitar, Piano, Other } // 0..6
pub const NUM_STEMS: usize = 6;

pub struct StemSet { pub paths: [PathBuf; NUM_STEMS] }

pub trait StemSeparator {
    fn available(&self) -> Result<(), String>;
    fn separate(
        &self,
        src: &Path,
        out_dir: &Path,
        cancelled: &dyn Fn() -> bool,   // same contract as rip_track_cancellable
        progress: &dyn Fn(u8),          // 0..=100 parsed from demucs stderr
    ) -> Result<StemSet, String>;
}

pub struct DemucsCli { pub command: PathBuf, pub model: String }
```

`DemucsCli::separate` spawns the child, polls it every 100 ms against the
cancel flag (SIGTERM on cancel — the ffmpeg-rip contract), parses `NN%`
progress ticks from stderr, and renames the six FLACs from
`<tmpdir>/htdemucs_6s/<track>/` into the cache atomically (`.part`
convention) so a killed run never leaves a half-cache. WMA inputs are
pre-flattened through the existing `resolve_playback_path()` WAV transcode.

**Upstream note:** `facebookresearch/demucs` is archived and its successor
`adefossez/demucs` is bugfix-only, while community forks such as
`demucs-next` track modern Python/PyTorch and claim large speedups. The
pip package spec is therefore a config field (`stems.package`), not a
hardcoded string, so users (and we) can switch forks without a release.

## 2. Engine provisioning — the "don't punish first-time setup" design

The user's concern: first-time setup already pays a slow library scan;
forcing a multi-GB PyTorch install on top of it is unacceptable. Two
sub-questions:

### 2a. Should we ship a pre-built (frozen) demucs?

**Precedent exists** — StemRoller, the best-known demucs GUI, freezes
demucs+torch into a standalone binary with cx_Freeze
(`cxfreeze main.py --packages=torch --includes=demucs.htdemucs`) and ships
it with the app. It works, but the costs are exactly what we'd inherit:

- **Per-OS × per-arch build matrix we own forever** (macOS arm64/x86_64,
  Linux x86_64) plus freeze-tool quirks StemRoller documents (manually
  copying `_soundfile_data`, ffmpeg expected on PATH).
- **Artifact weight:** torch alone is a ~600 MB wheel for CPU-only builds
  (~1.8–2.5 GB with CUDA), so every zytunes release would drag hundreds of
  MB of sidecar per platform for a feature many users won't touch.
- **No distribution channel to ride:** zytunes installs via
  `install.sh`/cargo as a small static-ish binary; there's no installer or
  .app bundle where a frozen sidecar would naturally live.

Verdict: **worse as the default**, acceptable as an escape hatch later
(`provision = "bundled"` reserved, pointing at a frozen build we'd publish
on our GitHub releases if demand appears).

### 2b. Default: lazy managed install via `uv`

`uv` is a single static Rust binary that creates isolated tool
environments (`uv tool install <pkg>` → `~/.local/share/uv/tools/…`) and
can bootstrap Python itself, so the user needs no working Python. Astral
documents pinning PyTorch to the CPU-only index
(`https://download.pytorch.org/whl/cpu`) to avoid the multi-GB CUDA stack.

Flow on first `M`-press when no engine is found:

1. Confirm overlay (reuses the existing y/n modal pattern): *"Stem playback
   needs the demucs engine (~1.5 GB, one-time). Install now? [y/n]"* —
   never silent, never at startup.
2. Background worker runs `uv tool install <stems.package>` with the
   CPU-only torch index (or CUDA/MPS default index when `stems.gpu = true`),
   streaming progress into the sync log; strip shows
   `installing engine… (one-time)`. Library scan, playback, sync are all
   unaffected — same worker isolation as separation itself.
3. On success the resolved binary path is written back to config
   (`stems.command`) so subsequent launches skip discovery entirely.
4. `uv` itself: use it if on PATH; otherwise offer to fetch the standalone
   binary into `~/.local/share/zytunes/bin/uv` (it's a single ~20 MB static
   executable) after the same y/n consent.

First model download (~150 MB for `htdemucs_6s`) happens on the first real
separation, handled by demucs itself into `~/.cache/torch`; the progress
line covers it.

### 2c. config.toml surface (all optional, sane defaults)

```toml
[stems]
provision = "auto"        # "auto" (uv-managed, default) | "manual" | future: "bundled"
command = ""              # manual mode: explicit demucs path (venv/pipx/system);
                          # auto mode writes the resolved path here after install
package = "demucs"        # pip spec used by auto-provision — swap for a fork
                          # (e.g. "demucs-next") without a zytunes release
model = "htdemucs_6s"     # cache-key participant; change forces re-separation
gpu = false               # auto mode: false → CPU-only torch index (small);
                          # true → default index (CUDA/MPS capable, much larger)
cache_max_gb = 10
```

`provision = "manual"` + `command` is the zero-magic path for users who
already run demucs in a venv. Everything the provisioner does is visible in
the sync log; nothing installs without the explicit y/n.

## 3. Stem cache

- Location: `$HOME/.cache/zytunes/stems/{key}/` next to the art cache;
  `key = hash(canonical source path)`, with `meta.json` recording the
  source `(mtime, size)` fingerprint plus model name — same invalidation
  scheme as the album-art cache. Re-tag/re-rip or model change → re-separate.
- Format: FLAC. Six stereo stems cost ~150–220 MB per 4-minute track, so
  the 10 GB cap holds roughly 50–65 separated tracks.
- **Pruning:** LRU by `meta.json` touch time, capped by
  `stems.cache_max_gb`; prune runs after each successful separation and
  logs evictions (no silent caps).
- Diagnostics route through the `Logger` pattern from `cache.rs` so
  warnings land in the SyncMessage channel, never raw stderr.

## 4. Playback engine (`src/tui/audio.rs`)

Key decision: **one mixed `Source`, not six sinks.** Parallel rodio players
can drift on pause/scrub and make TrackEnded ambiguous. Instead:

```rust
// src/tui/audio/stem_mix.rs
pub struct StemMixerSource {
    decoders: [Decoder<…>; NUM_STEMS], // demucs guarantees uniform 44.1k stereo
    gains: Arc<[AtomicU32; NUM_STEMS]>, // f32 bits; 1.0 = on, 0.0 = muted
    ramp: [f32; NUM_STEMS],             // smoothed per-stem gain
}
```

- `next()` sums `sample[i] * ramp[i]`; ramps chase the atomic targets over
  ~10 ms of samples so toggles are click-free.
- Ends when the longest decoder ends (shorter stems pad silence).
- The player loop treats it like any source: `p.empty()` still drives
  `TrackEnded`; `skip_duration` on the rebuilt mixer implements Scrub, so
  seeks stay sample-locked and toggle state survives (same gains `Arc`).

New `AudioCommand::PlayStems { stems: StemSet, gains: Arc<[AtomicU32; NUM_STEMS]> }`
— the app keeps the `Arc`, so toggling a stem is a lock-free store with no
channel round-trip. Exiting stem mode issues a normal `Play` of the original
file at the current position via the Scrub machinery.

## 5. Background worker wiring (`src/tui/background.rs`)

- `BgCommand::ProvisionStemEngine`, `BgCommand::SeparateStems { track_path }`,
  `BgCommand::CancelSeparation`
- `BgEvent::StemEngineProgress(String)`, `BgEvent::StemEngineReady { command: PathBuf }`,
  `BgEvent::StemProgress { path, pct }`, `BgEvent::StemsReady { path, stems }`,
  `BgEvent::StemsFailed { path, err }`
- Cache hit → instant `StemsReady`. Separation and provisioning run on a
  short-lived thread owned by the worker (library-scan pattern) with a
  dedicated cancel `AtomicBool` (independent of the rip flag) so they never
  block Connect/sync dispatch.

## 6. App state & keys (`src/tui/app.rs`, `app/keys.rs`)

New sub-struct on `App` (pattern: `DeviceState`/`SyncState`):

```rust
pub struct StemState {
    pub status: StemStatus, // Off | Provisioning | Separating { pct } | Active
    pub gains: Option<Arc<[AtomicU32; NUM_STEMS]>>,
    pub enabled: [bool; NUM_STEMS], // UI mirror for rendering
    pub for_path: Option<String>,
}
```

Key map — `s` (sort) and `S` (run sync queue) are taken; **`M`** ("mixer")
is free. Note the adjacency: lowercase `m` already opens the tag manager in
Library mode, so help/footer text must show both to keep them distinguishable:

- **`M`** while playing: cached stems → position-preserving swap into stem
  mode; engine missing → provisioning consent overlay (§2b); engine present
  but stems not cached → kick off separation (original keeps playing),
  auto-swap at the then-current position on `StemsReady`. `M` again while
  Active → back to normal single-file playback, position preserved.
- **`1`–`6` while Active**: toggle the six stems. `1`–`4` already carry
  global bindings (sidebar modes, panel jumps — `4` focuses the sync queue
  when non-empty), so `M` claims `1`–`6` and `Esc`/`M` releases them —
  implemented as a bool-returning `handle_stem_key` tried *before*
  `handle_global_key` in the existing modal-then-global cascade.
- Track changes reset `StemState` to Off (stems are per-track; a
  `stem_sticky` config option is possible follow-up).
- Gated to Library browse mode like the track-info popup; `M` in Device
  mode shows a toast.

Toggle path: flip `enabled[i]`, store `f32::to_bits(0.0|1.0)` into
`gains[i]`; the audio thread picks it up on the next sample block.

## 7. Rendering (`src/tui/ui/mod.rs`)

- `draw_now_playing` gains one line when `stems.status != Off`:
  Provisioning → install progress; Separating → percent (indeterminate
  spinner if no ticks parse); Active → the six cells with the narrow
  fallback described up top.
- `PLAYER_MIN_H` grows by 1 only while a stem line is shown, mirrored in
  `LayoutMetrics` and `should_show_player` so the auto-hide floor stays
  honest.
- Footer hint gains `M stems` in Library mode; help overlay documents
  `M` / `1`–`6`.

## 8. Failure & edge cases

- User declines the install consent → toast, status Off, nothing written.
- Provisioning fails (network, disk) → `StemsFailed`-style toast + sync-log
  detail; config untouched so the next `M` retries cleanly.
- Separation fails / cancelled → log + toast, status Off; original playback
  never interrupted. Cancelled ≠ failed in the summary (rip convention).
- Quit mid-separation → cancel flag SIGTERMs the child on TUI teardown;
  `.part` temp dir means no corrupt cache either way.
- Scrub while Separating → allowed (operates on the original file);
  auto-swap uses the then-current position.
- All six stems muted → valid silence; TrackEnded still fires.
- Model or package changed in config → `meta.json` mismatch forces
  re-separation (same philosophy as `CACHE_SCHEMA_VERSION`).

## 9. Implementation phases (each lands green: fmt, clippy -D warnings, tests)

1. **`src/stems.rs`** — `StemKind`/`StemSet`/`StemSeparator`, `DemucsCli`
   with cancel + progress parse, cache layout + fingerprint invalidation +
   LRU prune. Tests: cache keying, meta round-trip, prune ordering,
   stderr progress parsing (fixture), 6-path assembly from demucs layout.
2. **Provisioner** — `provision.rs`: engine discovery (config command →
   PATH → uv tools dir), uv detection/bootstrap, install command builder
   (CPU vs GPU index), config write-back. Tests: discovery precedence,
   command construction per config permutation (no network in tests).
3. **Audio engine** — `StemMixerSource` + `PlayStems` + stems-aware Scrub.
   Tests with generated WAV fixtures: mix correctness, click-free ramp,
   silence-padding on length skew, seek, all-muted end detection.
4. **Background wiring** — commands/events above, dedicated cancel flag,
   cache-hit fast path, via the existing worker test harness pattern.
5. **App + UI** — `StemState`, consent overlay, `M`/`1`–`6` in the key
   cascade, strip + narrow fallback, layout floor bump, footer/help.
   Tests: key gating and release, Device-mode toast, state reset on track
   change, `should_show_player` floor with the extra line.
6. **Docs** — config reference, CLAUDE.md section (demucs/uv are
   use-time-optional external tools alongside ffmpeg), README blurb.

Phases 1–3 are the substance; 4–6 are wiring. No workspace-structure
changes; everything lives in the root crate.

## As-built deviations

Implemented across phases 1–6 (branch `claude/stem-splitting-player-v1fqoc`);
the code deviates from the sections above in these deliberate ways:

- **`1`–`6` are claimed strictly while `Active`; `Esc` does not release
  them.** §6 proposed an Esc-releasable claim, but a "claim released while
  stems still play" half-state is a footgun — instead the claim IS the
  Active status, and `M` is the single exit.
- **`M` during Provisioning/Separating cancels the job** (not covered
  above) — one key drives the whole cycle.
- **Track changes cancel a running separation** (revised after field
  use — the original finish-into-cache behaviour left the worker busy,
  so splitting the next track bounced off the busy guard). Worker-side,
  stem jobs are serialised by a job mutex with per-job cancel tokens:
  a newly dispatched job supersedes the previous one instead of being
  rejected. Engine installs still survive track changes.
- **`PLAYER_MIN_H` is unchanged.** §7 planned to grow the auto-hide floor
  with the strip; in practice the browser's `Min(8)` absorbs the one-row
  bump, so only the panel height (9→10) varies.
- **`AudioCommand::PlayStems` never shipped; transitions are
  `AudioCommand::SwapSource`.** §4's Play-the-mixer + Scrub-to-position
  entry (and Play+Scrub exit) had an audible half-second hole while six
  decoders opened and sought. Instead one `SwapSource { target }` command
  covers both directions: the audio thread builds and pre-seeks the
  incoming source to its own live clock while the old source keeps
  playing, then cuts over under a 15 ms fade. Seeks go through
  `Source::try_seek` (accurate mode) with an eager-decode fallback.
- **Post-review hardening (same branch).** Stem jobs are identified by a
  monotonic generation echoed on every event — track paths are not
  identity, and a cancelled job's late terminal event must not wipe a
  same-track re-request. Track changes preserve a running install's
  Provisioning status (not just its process). Engine/installer children
  run in their own process groups, group-killed on cancel and swept by
  the TUI quit path (`kill_active_stem_children`) so macOS quits can't
  orphan demucs against the HuggingFace model lock. Swap/scrub failures
  keep the old source playing instead of tearing playback down.

## References

- StemRoller's frozen-demucs packaging (cx_Freeze, `--packages=torch`,
  `_soundfile_data` copy, ffmpeg-on-PATH):
  <https://github.com/stemrollerapp/demucs-cxfreeze>
- uv tool environments (isolated installs under `~/.local/share/uv/tools`,
  PATH-linked executables): <https://docs.astral.sh/uv/concepts/tools/>
- uv managed Python (no system Python required):
  <https://docs.astral.sh/uv/guides/install-python/>
- uv × PyTorch index selection (CPU-only vs CUDA wheels):
  <https://docs.astral.sh/uv/guides/integration/pytorch/>
- PyTorch wheel weight (why CPU-only index matters):
  <https://github.com/pytorch/pytorch/issues/17621>
- demucs upstream status (archived at Meta; successor bugfix-only):
  <https://github.com/facebookresearch/demucs>,
  <https://github.com/adefossez/demucs>
- Maintained community fork option for `stems.package`:
  <https://github.com/Ryan5453/demucs-next>
