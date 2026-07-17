//! Stem separation — recipes, engines, and the on-disk stem cache.
//!
//! Separation is an offline, cached, background step; nothing here is
//! invoked on the audio path. The TUI's background worker calls
//! [`cached_stems`] first and only runs [`StemSeparator::separate`] on a
//! miss; the resulting FLACs land in the cache via [`store_stems`].
//!
//! A [`RecipeKind`] names a pipeline: `demucs` (single [`DemucsCli`]
//! pass), or the audio-separator recipes `hq` / `hq-harmony` (a
//! [`CascadeSeparator`] executing [`RecipePass`] slices — Roformer
//! vocals, demucs band, optionally a karaoke lead/backing split). Each
//! recipe declares an ordered stem *layout* that drives filenames, UI
//! cells, digit keys, and gain indices alike.
//!
//! Shelling out mirrors the ffmpeg precedent (video-sync, CD rip): the
//! engines are optional external tools, checked at use time with a
//! friendly error; the trait keeps the worker and cache agnostic so an
//! in-process backend can slot in later. See
//! `docs/stem-splitting-plan.md` and `docs/stem-quality-upgrade-plan.md`.

mod process;
pub mod provision;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::cache::{FileFingerprint, Logger};
pub use process::{kill_active_stem_children, parse_progress_percent};
use process::{run_engine_process, EngineHooks, EngineOutcome};

/// Capacity ceiling for per-stem state (gains, enabled flags). Fixed so
/// the lock-free `[AtomicU32; MAX_STEMS]` gains array never reallocates;
/// recipes occupy `layout().len()` slots and the rest idle muted.
pub const MAX_STEMS: usize = 8;

/// A named, versioned separation pipeline — the unit `[stems] recipe`
/// selects. A recipe decides which engine runs ([`RecipeKind::engine`]),
/// what the cache entry is keyed as ([`RecipeKind::cache_id`]), and (for
/// multi-pass recipes) which model passes produce which stems. See
/// `docs/stem-quality-upgrade-plan.md` §3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecipeKind {
    /// The original single-pass demucs separation (default).
    Demucs,
    /// BS-Roformer vocals + `htdemucs_6s` band cascade via
    /// audio-separator — same six stems, audibly better vocals.
    Hq,
    /// The `hq` cascade plus a Mel-Roformer karaoke pass splitting the
    /// vocals into lead + backing (seven stems).
    HqHarmony,
}

impl RecipeKind {
    /// The engine executable this recipe drives.
    pub fn engine(self) -> provision::EngineKind {
        match self {
            RecipeKind::Demucs => provision::EngineKind::Demucs,
            RecipeKind::Hq | RecipeKind::HqHarmony => provision::EngineKind::AudioSeparator,
        }
    }

    /// Cache identity stored in [`StemCacheMeta::model`] and compared for
    /// equality on lookup. The demucs recipe's id IS the configured model
    /// string so every pre-recipe cache entry stays a hit; multi-pass
    /// recipes carry an explicit version — bump it when swapping a
    /// checkpoint so stale mixes re-separate.
    pub fn cache_id(self, demucs_model: &str) -> String {
        match self {
            RecipeKind::Demucs => demucs_model.to_string(),
            RecipeKind::Hq => "hq/v1".to_string(),
            RecipeKind::HqHarmony => "hq-harmony/v1".to_string(),
        }
    }

    /// The ordered stems this recipe produces — the single source of
    /// truth for UI cells, digit keys, gain indices, and stem filenames.
    pub fn layout(self) -> &'static [StemKind] {
        match self {
            RecipeKind::Demucs | RecipeKind::Hq => SIX_STEM_LAYOUT,
            RecipeKind::HqHarmony => HARMONY_STEM_LAYOUT,
        }
    }

    /// The `[stems] recipe` config value (also the `Display` form).
    pub fn config_value(self) -> &'static str {
        match self {
            RecipeKind::Demucs => "demucs",
            RecipeKind::Hq => "hq",
            RecipeKind::HqHarmony => "hq-harmony",
        }
    }
}

impl std::fmt::Display for RecipeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.config_value())
    }
}

impl std::str::FromStr for RecipeKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "demucs" => Ok(RecipeKind::Demucs),
            "hq" => Ok(RecipeKind::Hq),
            "hq-harmony" => Ok(RecipeKind::HqHarmony),
            other => Err(other.to_string()),
        }
    }
}

/// File extension the cache stores stems as. `--flac` keeps them lossless
/// at roughly half the disk of WAV.
pub const STEM_EXT: &str = "flac";

/// One separated source. Which kinds a track splits into — and their
/// order (UI cells, digit keys, gain indices, [`StemSet::paths`]) — is
/// the recipe's *layout* ([`RecipeKind::layout`]); a kind's position in
/// its layout is its index everywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StemKind {
    Vocals,
    /// Lead vocal line (harmony recipes split [`StemKind::Vocals`]).
    LeadVocals,
    /// Backing vocals / harmonies.
    BackingVocals,
    Drums,
    Bass,
    Guitar,
    Piano,
    Other,
}

/// The classic six-source order every demucs-family separation uses.
pub const SIX_STEM_LAYOUT: &[StemKind] = &[
    StemKind::Vocals,
    StemKind::Drums,
    StemKind::Bass,
    StemKind::Guitar,
    StemKind::Piano,
    StemKind::Other,
];

/// The seven-stem harmony order: the vocal take split into lead +
/// backing, then the band.
pub const HARMONY_STEM_LAYOUT: &[StemKind] = &[
    StemKind::LeadVocals,
    StemKind::BackingVocals,
    StemKind::Drums,
    StemKind::Bass,
    StemKind::Guitar,
    StemKind::Piano,
    StemKind::Other,
];

impl StemKind {
    /// The filename (sans extension) this stem is stored as.
    pub fn file_stem(self) -> &'static str {
        match self {
            StemKind::Vocals => "vocals",
            StemKind::LeadVocals => "lead",
            StemKind::BackingVocals => "backing",
            StemKind::Drums => "drums",
            StemKind::Bass => "bass",
            StemKind::Guitar => "guitar",
            StemKind::Piano => "piano",
            StemKind::Other => "other",
        }
    }

    /// Full label for help text and the wide stem strip.
    pub fn label(self) -> &'static str {
        match self {
            StemKind::Vocals => "Vocals",
            StemKind::LeadVocals => "Lead Vocals",
            StemKind::BackingVocals => "Backing Vocals",
            StemKind::Drums => "Drums",
            StemKind::Bass => "Bass",
            StemKind::Guitar => "Guitar",
            StemKind::Piano => "Piano",
            StemKind::Other => "Other",
        }
    }

    /// Three-character label for the now-playing strip ("LdV"/"BkV"
    /// follow the sheet-notation LV/BV convention).
    pub fn short_label(self) -> &'static str {
        match self {
            StemKind::Vocals => "Voc",
            StemKind::LeadVocals => "LdV",
            StemKind::BackingVocals => "BkV",
            StemKind::Drums => "Drm",
            StemKind::Bass => "Bas",
            StemKind::Guitar => "Gtr",
            StemKind::Piano => "Pno",
            StemKind::Other => "Oth",
        }
    }
}

/// The stem files for one track, ordered by `layout` — `paths[i]` is
/// `layout[i]`'s file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StemSet {
    pub layout: &'static [StemKind],
    pub paths: Vec<PathBuf>,
}

impl StemSet {
    /// Assemble `dir/{lead,backing,…}.{ext}` paths in `layout` order —
    /// engine work dirs and cache entries share this shape.
    pub fn from_layout(dir: &Path, ext: &str, layout: &'static [StemKind]) -> StemSet {
        StemSet {
            layout,
            paths: layout
                .iter()
                .map(|k| dir.join(format!("{}.{ext}", k.file_stem())))
                .collect(),
        }
    }

    /// True when every stem file exists on disk.
    pub fn all_exist(&self) -> bool {
        self.paths.iter().all(|p| p.is_file())
    }
}

/// Shared per-stem gain targets: `f32` bits stored in `AtomicU32`,
/// indexed by layout position. Fixed [`MAX_STEMS`] capacity so the array
/// stays lock-free and allocation-free; slots beyond the active layout
/// idle at 0.0. The app owns one clone and the audio thread's mixer
/// another; toggling a stem is a lock-free store the mixer picks up on
/// its next frame — no channel round-trip.
pub type StemGains = Arc<[std::sync::atomic::AtomicU32; MAX_STEMS]>;

/// Fresh gains with each of the first `enabled.len()` stems at `1.0`
/// (enabled) or `0.0` (muted); remaining capacity muted.
pub fn new_stem_gains(enabled: &[bool]) -> StemGains {
    Arc::new(std::array::from_fn(|i| {
        let on = enabled.get(i).copied().unwrap_or(false);
        std::sync::atomic::AtomicU32::new(if on { 1.0f32 } else { 0.0f32 }.to_bits())
    }))
}

/// Set stem `index` fully on or off. Muting is a gain of `0.0`; the
/// mixer's ramp turns the step into a short fade.
pub fn set_stem_gain(gains: &StemGains, index: usize, on: bool) {
    gains[index].store(
        if on { 1.0f32 } else { 0.0f32 }.to_bits(),
        std::sync::atomic::Ordering::Relaxed,
    );
}

/// Read the current target gain for stem `index`.
pub fn stem_gain(gains: &StemGains, index: usize) -> f32 {
    f32::from_bits(gains[index].load(std::sync::atomic::Ordering::Relaxed))
}

#[derive(Debug)]
pub enum StemError {
    /// The engine binary could not be spawned (missing, not executable).
    Spawn(String),
    /// The engine exited non-zero. `stderr` carries its diagnostic tail;
    /// `exit_code` is `None` when terminated by signal.
    EngineFailed {
        exit_code: Option<i32>,
        stderr: String,
    },
    /// User cancelled mid-separation. Partial output is the caller's to
    /// discard (it lives in the caller-supplied `out_dir`, never the cache).
    Cancelled,
    /// The engine exited zero but an expected stem file is missing —
    /// usually a model/filename mismatch.
    MissingOutput(PathBuf),
    Io(String),
}

impl std::fmt::Display for StemError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StemError::Spawn(e) => write!(f, "failed to spawn stem engine: {e}"),
            StemError::EngineFailed {
                exit_code: Some(c),
                stderr,
            } => write!(f, "stem engine exited with status {c}: {stderr}"),
            StemError::EngineFailed {
                exit_code: None,
                stderr,
            } => write!(f, "stem engine terminated without status: {stderr}"),
            StemError::Cancelled => write!(f, "separation cancelled by user"),
            StemError::MissingOutput(p) => {
                write!(f, "stem engine produced no {}", p.display())
            }
            StemError::Io(e) => write!(f, "stem io error: {e}"),
        }
    }
}

impl std::error::Error for StemError {}

/// A stem-separation engine. `DemucsCli` is the only implementation today;
/// the trait keeps the background worker and cache agnostic so an
/// in-process backend can replace the subprocess later.
pub trait StemSeparator {
    /// Cheap availability probe, called at `M`-press time (never startup).
    /// The `Err` string is shown to the user verbatim.
    fn available(&self) -> Result<(), String>;

    /// Separate `src` into `out_dir`, blocking until done.
    ///
    /// `cancelled` is polled every ~100 ms and kills the engine when it
    /// returns true (the `rip_track_cancellable` contract). `progress`
    /// receives 0..=100 ticks parsed from the engine's output; callers
    /// must tolerate sparse or absent ticks. `on_line` receives the
    /// engine's informational output (model download notices, per-track
    /// banners, warnings — progress bars excluded) so a stalled or
    /// misbehaving engine is diagnosable from the app's log instead of
    /// silently eating CPU.
    fn separate(
        &self,
        src: &Path,
        out_dir: &Path,
        cancelled: &dyn Fn() -> bool,
        progress: &dyn Fn(u8),
        on_line: &dyn Fn(&str),
    ) -> Result<StemSet, StemError>;
}

/// Shell-out to the `demucs` CLI (or a compatible fork — the binary and
/// model both come from `[stems]` config).
pub struct DemucsCli {
    pub command: PathBuf,
    pub model: String,
    /// Stem layout the recipe expects the model to emit (from
    /// [`RecipeKind::layout`]) — the recipe owns the layout decision, not
    /// the engine. A model that emits fewer stems (e.g. 4-stem
    /// `htdemucs` set via `[stems] model`) fails the post-run check with
    /// a clear model/layout mismatch instead of a bare missing-file.
    pub layout: &'static [StemKind],
}

/// Build the demucs argv tail: `-n <model> --flac -o <out_dir> <input>`.
///
/// `--flac` selects lossless output at roughly half the disk of demucs'
/// default WAV; the muxer choice must agree with [`STEM_EXT`] because
/// [`expected_output_dir`] assembles the paths from it.
pub fn build_demucs_args(model: &str, out_dir: &Path, input: &Path) -> Vec<String> {
    vec![
        "-n".into(),
        model.to_string(),
        "--flac".into(),
        "-o".into(),
        out_dir.to_string_lossy().into_owned(),
        input.to_string_lossy().into_owned(),
    ]
}

/// Where demucs writes the stems for `input`: `<out_dir>/<model>/<track>/`
/// where `<track>` is the input's file stem.
pub fn expected_output_dir(out_dir: &Path, model: &str, input: &Path) -> PathBuf {
    let track = input.file_stem().unwrap_or_default().to_string_lossy();
    out_dir.join(model).join(track.as_ref())
}

/// Probe an engine binary by running it with a fast, side-effect-free
/// flag and mapping the three outcomes (ran clean / ran and failed /
/// couldn't spawn) onto one error shape. Shared by both engines so a fix
/// to the probe (wording, a future hang guard) lands once.
fn probe_engine(command: &Path, flag: &str) -> Result<(), String> {
    match std::process::Command::new(command)
        .arg(flag)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
    {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!(
            "{} {flag} exited with {:?}",
            command.display(),
            s.code()
        )),
        Err(e) => Err(format!(
            "stem engine not found at {}: {e}",
            command.display()
        )),
    }
}

impl StemSeparator for DemucsCli {
    fn available(&self) -> Result<(), String> {
        // `--help` is argparse-only (no torch import) so this stays fast.
        probe_engine(&self.command, "--help")
    }

    fn separate(
        &self,
        src: &Path,
        out_dir: &Path,
        cancelled: &dyn Fn() -> bool,
        progress: &dyn Fn(u8),
        on_line: &dyn Fn(&str),
    ) -> Result<StemSet, StemError> {
        std::fs::create_dir_all(out_dir).map_err(|e| StemError::Io(e.to_string()))?;
        let mut cmd = std::process::Command::new(&self.command);
        cmd.args(build_demucs_args(&self.model, out_dir, src));
        // demucs writes its tqdm progress to stderr and informational
        // lines ("Selected model…", "Separating track…", download
        // notices) to stdout; the shared driver drains both pipes and
        // owns the cancel poll, group kill, and quit-teardown registry.
        let hooks = EngineHooks {
            cancelled,
            on_progress: Some(progress),
            on_line,
        };
        match run_engine_process(&mut cmd, &hooks) {
            Err(e) => Err(StemError::Spawn(e)),
            Ok(EngineOutcome::Cancelled) => Err(StemError::Cancelled),
            Ok(EngineOutcome::Failed { exit_code, tail }) => Err(StemError::EngineFailed {
                exit_code,
                stderr: tail,
            }),
            Ok(EngineOutcome::Success) => {
                let dir = expected_output_dir(out_dir, &self.model, src);
                let set = StemSet::from_layout(&dir, STEM_EXT, self.layout);
                for p in &set.paths {
                    if !p.is_file() {
                        return Err(StemError::MissingOutput(p.clone()));
                    }
                }
                progress(100);
                Ok(set)
            }
        }
    }
}

/// The demucs 6-source model as audio-separator names it.
pub const AUDIO_SEPARATOR_HTDEMUCS_MODEL: &str = "htdemucs_6s.yaml";

/// Shell-out to `python-audio-separator`'s `audio-separator` CLI — the
/// engine behind the `hq`/`hq-harmony` recipes. One invocation per model
/// pass; `--custom_output_names` makes every pass emit our canonical
/// stem filenames directly into `--output_dir`, so there is no
/// engine-layout dance like demucs' model/track subdirectories.
pub struct AudioSeparatorCli {
    pub command: PathBuf,
    /// Checkpoint download cache passed as `--model_file_dir`. The
    /// engine's default is under `/tmp` (wiped on reboot), so callers
    /// pass [`default_model_file_dir`] to persist checkpoints.
    pub model_file_dir: PathBuf,
}

/// Where audio-separator model checkpoints persist:
/// `~/.cache/zytunes/models`. Lives beside the stem cache and — like it —
/// deliberately ignores `ZYTUNES_CACHE_DIR`: checkpoints are derived,
/// shareable data every worktree should reuse (they run 200 MB–1 GB
/// each).
pub fn default_model_file_dir() -> Option<PathBuf> {
    Some(crate::paths::zytunes_cache_root()?.join("models"))
}

/// Every model file a current recipe can reference. Derived from the same
/// pinned constants the recipes are built on, so a checkpoint swap
/// automatically retires the old file for [`prune_model_cache`].
pub fn pinned_model_files() -> [&'static str; 3] {
    [
        AUDIO_SEPARATOR_HTDEMUCS_MODEL,
        BS_ROFORMER_VOCALS_MODEL,
        MEL_ROFORMER_KARAOKE_MODEL,
    ]
}

/// Evict retired model checkpoints from `model_dir`.
///
/// A checkpoint swap (bumping [`BS_ROFORMER_VOCALS_MODEL`] etc.) orphans
/// the previous 0.2–1 GB file forever — nothing else walks this dir. The
/// policy is pinned-set cleanup rather than LRU: Roformer checkpoints are
/// pinned constants, not user config, so a `.ckpt` whose filename is not
/// in `pinned` — and whose stem no pinned file owns — is provably
/// retired. A retired checkpoint is deleted together with its
/// `.yaml`/`.yml`/`.json` sidecar configs (same stem); any other file is
/// never touched: audio-separator also stores engine-managed data here
/// (demucs weight segments, registry json, download locks) whose names
/// we don't control, and deleting those would force silent re-downloads.
///
/// Retirement only covers `.ckpt` models. If the yaml-named
/// [`AUDIO_SEPARATOR_HTDEMUCS_MODEL`] is ever swapped, the old yaml and
/// its hash-named demucs weight segments are NOT reclaimed — hash-named
/// weights can't be attributed to a model by this policy, so a future
/// model bump must clean those up by other means.
pub fn prune_model_cache(model_dir: &Path, pinned: &[&str], log: &Logger) {
    let Ok(entries) = std::fs::read_dir(model_dir) else {
        return;
    };
    let files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();

    let has_ext = |p: &Path, ext: &str| {
        p.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case(ext))
    };

    // Stems owned by pinned files are never retire-able: a stray
    // non-pinned `htdemucs_6s.ckpt` beside the pinned `htdemucs_6s.yaml`
    // must not drag the live model's sidecars into the sweep. Leaking
    // the stray file is the safe failure mode.
    let pinned_stems: std::collections::HashSet<&std::ffi::OsStr> = pinned
        .iter()
        .filter_map(|n| Path::new(n).file_stem())
        .collect();

    // A retired stem: a .ckpt present on disk whose filename no current
    // recipe references and whose stem no pinned file owns.
    let retired: std::collections::HashSet<&std::ffi::OsStr> = files
        .iter()
        .filter(|p| has_ext(p, "ckpt"))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| !pinned.contains(&n))
        })
        .filter_map(|p| p.file_stem())
        .filter(|s| !pinned_stems.contains(s))
        .collect();
    if retired.is_empty() {
        return;
    }

    for path in &files {
        if !path.file_stem().is_some_and(|s| retired.contains(s)) {
            continue;
        }
        // Only the checkpoint and its config sidecars — a same-stem file
        // with any other extension is engine-managed and stays.
        if !(has_ext(path, "ckpt")
            || has_ext(path, "yaml")
            || has_ext(path, "yml")
            || has_ext(path, "json"))
        {
            continue;
        }
        // Belt-and-braces: never delete a file whose full name is pinned,
        // even if it shares a stem with a retired checkpoint.
        if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| pinned.contains(&n))
        {
            continue;
        }
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        match std::fs::remove_file(path) {
            Ok(()) => log(&format!(
                "model cache: evicted retired {} ({:.1} MB)",
                path.display(),
                size as f64 / (1024.0 * 1024.0)
            )),
            Err(e) => log(&format!(
                "model cache: failed to evict {}: {e}",
                path.display()
            )),
        }
    }
}

/// Build the audio-separator argv for one model pass. `output_names`
/// maps the model's stem names (e.g. `"Vocals"`) to the file stems we
/// want on disk (e.g. `"vocals"`), serialized as the JSON
/// `--custom_output_names` expects. FLAC output matches [`STEM_EXT`].
pub fn build_audio_separator_args(
    model: &str,
    out_dir: &Path,
    model_file_dir: &Path,
    output_names: &[(&str, &str)],
    input: &Path,
) -> Vec<String> {
    let names: serde_json::Map<String, serde_json::Value> = output_names
        .iter()
        .map(|(stem, file)| (stem.to_string(), serde_json::Value::from(*file)))
        .collect();
    vec![
        input.to_string_lossy().into_owned(),
        "-m".into(),
        model.to_string(),
        "--output_dir".into(),
        out_dir.to_string_lossy().into_owned(),
        "--output_format".into(),
        STEM_EXT.into(),
        "--model_file_dir".into(),
        model_file_dir.to_string_lossy().into_owned(),
        "--custom_output_names".into(),
        serde_json::Value::Object(names).to_string(),
    ]
}

/// The BS-Roformer vocals/instrumental checkpoint the `hq` recipes pin.
/// Community-consensus best vocal model; also audio-separator's own
/// default. Swapping it requires bumping the recipe version in
/// [`RecipeKind::cache_id`] so cached mixes re-separate.
pub const BS_ROFORMER_VOCALS_MODEL: &str = "model_bs_roformer_ep_317_sdr_12.9755.ckpt";

/// The Mel-Roformer karaoke checkpoint (aufr33/viperx) that splits an
/// isolated vocal take into lead + backing. Fed pass 1's full vocals —
/// on vocals-only input its "Instrumental" output IS the backing
/// vocals. Candidate for an A/B swap against the becruily karaoke model
/// once live listening tests run; swapping bumps the recipe version.
pub const MEL_ROFORMER_KARAOKE_MODEL: &str =
    "mel_band_roformer_karaoke_aufr33_viperx_sdr_10.1956.ckpt";

/// What feeds a [`RecipePass`]: the original source track, or a file an
/// earlier pass produced into the work dir (named by file stem).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PassInput {
    Source,
    Produced(&'static str),
}

/// One model pass of a multi-pass recipe. Passes are data, not code —
/// a recipe is a slice of these and [`CascadeSeparator`] executes them
/// in order into one shared work dir.
pub struct RecipePass {
    pub model: &'static str,
    pub input: PassInput,
    /// Engine stem name → output file stem (the `--custom_output_names`
    /// mapping). Later passes may consume earlier passes' outputs by
    /// file stem via [`PassInput::Produced`].
    pub output_names: &'static [(&'static str, &'static str)],
    /// Progress window `(start, end)` this pass's 0–100 ticks map onto,
    /// so the strip advances smoothly across the whole cascade instead
    /// of rewinding at each pass boundary.
    pub window: (u8, u8),
    /// Human label for the per-pass log line.
    pub label: &'static str,
}

/// The `hq` recipe: BS-Roformer pulls vocals off the source, then
/// htdemucs_6s separates the band from the devocalized instrumental —
/// the community-standard cascade. Pass 2's own vocals output is
/// near-silence by construction and maps to a discard name so it can't
/// clobber the real vocals stem.
pub fn hq_recipe_passes() -> &'static [RecipePass] {
    static PASSES: [RecipePass; 2] = [
        RecipePass {
            model: BS_ROFORMER_VOCALS_MODEL,
            input: PassInput::Source,
            output_names: &[("Vocals", "vocals"), ("Instrumental", "instrumental")],
            window: (0, 45),
            label: "vocals (BS-Roformer)",
        },
        RecipePass {
            model: AUDIO_SEPARATOR_HTDEMUCS_MODEL,
            input: PassInput::Produced("instrumental"),
            output_names: &[
                ("Vocals", "band_vocals"),
                ("Drums", "drums"),
                ("Bass", "bass"),
                ("Guitar", "guitar"),
                ("Piano", "piano"),
                ("Other", "other"),
            ],
            window: (45, 100),
            label: "band (htdemucs_6s over the instrumental)",
        },
    ];
    &PASSES
}

/// The `hq-harmony` recipe: the hq cascade with the vocal take routed
/// to an intermediate, then split into lead + backing by the karaoke
/// model. Window weights assume the two Roformer passes dominate on
/// CPU; tune against live timings.
pub fn hq_harmony_recipe_passes() -> &'static [RecipePass] {
    static PASSES: [RecipePass; 3] = [
        RecipePass {
            model: BS_ROFORMER_VOCALS_MODEL,
            input: PassInput::Source,
            output_names: &[("Vocals", "vocals_full"), ("Instrumental", "instrumental")],
            window: (0, 35),
            label: "vocals (BS-Roformer)",
        },
        RecipePass {
            model: AUDIO_SEPARATOR_HTDEMUCS_MODEL,
            input: PassInput::Produced("instrumental"),
            output_names: &[
                ("Vocals", "band_vocals"),
                ("Drums", "drums"),
                ("Bass", "bass"),
                ("Guitar", "guitar"),
                ("Piano", "piano"),
                ("Other", "other"),
            ],
            window: (35, 70),
            label: "band (htdemucs_6s over the instrumental)",
        },
        RecipePass {
            model: MEL_ROFORMER_KARAOKE_MODEL,
            input: PassInput::Produced("vocals_full"),
            output_names: &[("Vocals", "lead"), ("Instrumental", "backing")],
            window: (70, 100),
            label: "lead/backing split (Mel-Roformer karaoke)",
        },
    ];
    &PASSES
}

impl AudioSeparatorCli {
    /// Run one model pass: `input` separated by `model` into `out_dir`,
    /// stems named per `output_names`. [`CascadeSeparator`] calls this
    /// once per pass with its own progress-windowed hooks.
    pub(crate) fn run_pass(
        &self,
        input: &Path,
        model: &str,
        out_dir: &Path,
        output_names: &[(&str, &str)],
        hooks: &EngineHooks<'_>,
    ) -> Result<(), StemError> {
        std::fs::create_dir_all(out_dir).map_err(|e| StemError::Io(e.to_string()))?;
        std::fs::create_dir_all(&self.model_file_dir).map_err(|e| StemError::Io(e.to_string()))?;
        let mut cmd = std::process::Command::new(&self.command);
        cmd.args(build_audio_separator_args(
            model,
            out_dir,
            &self.model_file_dir,
            output_names,
            input,
        ));
        match run_engine_process(&mut cmd, hooks) {
            Err(e) => Err(StemError::Spawn(e)),
            Ok(EngineOutcome::Cancelled) => Err(StemError::Cancelled),
            Ok(EngineOutcome::Failed { exit_code, tail }) => Err(StemError::EngineFailed {
                exit_code,
                stderr: tail,
            }),
            Ok(EngineOutcome::Success) => Ok(()),
        }
    }

    /// Cheap-ish availability probe. `--version` is the lightest no-op
    /// the CLI offers; it still imports the package (unlike demucs'
    /// argparse-only --help), so the first call after an install can
    /// take a few seconds — it runs on the background worker, never the
    /// UI thread.
    pub fn available(&self) -> Result<(), String> {
        probe_engine(&self.command, "--version")
    }
}

/// Executes a recipe's passes in order through one [`AudioSeparatorCli`]
/// into a shared work dir. Intermediates (e.g. the instrumental, the
/// discarded band-vocals residue) are left in the work dir — the worker
/// removes it on every outcome, and [`store_stems`] moves only the six
/// canonical files into the cache.
pub struct CascadeSeparator {
    pub engine: AudioSeparatorCli,
    pub passes: &'static [RecipePass],
    /// Stem order of the final set — the recipe's layout.
    pub layout: &'static [StemKind],
}

impl StemSeparator for CascadeSeparator {
    fn available(&self) -> Result<(), String> {
        self.engine.available()
    }

    fn separate(
        &self,
        src: &Path,
        out_dir: &Path,
        cancelled: &dyn Fn() -> bool,
        progress: &dyn Fn(u8),
        on_line: &dyn Fn(&str),
    ) -> Result<StemSet, StemError> {
        std::fs::create_dir_all(out_dir).map_err(|e| StemError::Io(e.to_string()))?;
        let total = self.passes.len();
        for (i, pass) in self.passes.iter().enumerate() {
            let input = match pass.input {
                PassInput::Source => src.to_path_buf(),
                PassInput::Produced(stem) => {
                    let p = out_dir.join(format!("{stem}.{STEM_EXT}"));
                    if !p.is_file() {
                        // An earlier pass exited zero without writing the
                        // file this pass consumes — a model/name mismatch
                        // worth naming precisely.
                        return Err(StemError::MissingOutput(p));
                    }
                    p
                }
            };
            on_line(&format!("pass {}/{total}: {}", i + 1, pass.label));
            let (lo, hi) = pass.window;
            let windowed = |p: u8| {
                let span = hi.saturating_sub(lo) as u32;
                progress(lo + (span * p.min(100) as u32 / 100) as u8);
            };
            self.engine.run_pass(
                &input,
                pass.model,
                out_dir,
                pass.output_names,
                &EngineHooks {
                    cancelled,
                    on_progress: Some(&windowed),
                    on_line,
                },
            )?;
            progress(hi);
        }
        let set = StemSet::from_layout(out_dir, STEM_EXT, self.layout);
        for p in &set.paths {
            if !p.is_file() {
                return Err(StemError::MissingOutput(p.clone()));
            }
        }
        progress(100);
        Ok(set)
    }
}

// ---------------------------------------------------------------------------
// Stem cache
// ---------------------------------------------------------------------------

/// Sidecar written last into each cache entry; its presence marks the
/// entry valid, its `(mtime, size, model)` triple decides staleness, and
/// its own file mtime is the LRU clock for [`prune_stem_cache`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StemCacheMeta {
    /// Absolute path of the source track (diagnostic only — the entry is
    /// keyed by its hash, so collisions resolve via fingerprint mismatch).
    pub source_path: String,
    pub fingerprint: FileFingerprint,
    pub model: String,
}

const META_NAME: &str = "meta.json";

/// Default stem cache root. Lives under `$HOME/.cache/zytunes` beside the
/// dirlib and art caches, and — like them — deliberately ignores
/// `ZYTUNES_CACHE_DIR`: stems are derived purely from source audio, so
/// sibling worktrees pointed at the same `~/Music` should share one copy
/// of these large files.
pub fn default_stem_cache_dir() -> Option<PathBuf> {
    Some(crate::paths::zytunes_cache_root()?.join("stems"))
}

/// Cache entry directory name for a `(source path, recipe cache id)`
/// pair — the path hashed with the same `DefaultHasher` scheme the
/// dirlib cache uses for scan roots, suffixed with the sanitised cache
/// id. Keying on the pair (rather than the path alone) lets recipes
/// coexist per-track: flipping `[stems] recipe` for an A/B comparison
/// hits both ways instead of paying a full re-separation on every flip.
pub fn stem_cache_key(source_path: &str, cache_id: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    source_path.hash(&mut hasher);
    format!("{:016x}-{}", hasher.finish(), sanitize_cache_id(cache_id))
}

/// Filesystem-safe form of a recipe cache id (`"hq/v1"` → `"hq-v1"`).
/// Kept readable rather than hashed so a cache dir listing answers
/// "which recipe is this" at a glance.
///
/// `.` is deliberately mapped to `-` too: a dot in the key could make an
/// entry name end in `.tmp`, colliding with the stage-dir namespace —
/// `prune_stem_cache`'s stage sweep would delete the entry its own store
/// just wrote. No shipped cache id contains a dot, so nothing regresses.
///
/// The mapping is lossy, so distinct raw ids can share a dirname (a
/// demucs `model = "hq-v1"` collides with the `hq` recipe's `"hq/v1"`).
/// That is thrash, not corruption: `cached_stems` checks the raw
/// `meta.model`, so the colliding configs evict each other on every flip
/// but the wrong stems are never served.
fn sanitize_cache_id(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// A pre-recipe-key entry dirname: the bare 16-hex path hash, no cache-id
/// suffix.
fn is_legacy_key(name: &str) -> bool {
    name.len() == 16 && name.chars().all(|c| c.is_ascii_hexdigit())
}

/// Lossless migration of pre-recipe-key cache entries to the
/// `{path_hash}-{cache_id}` naming. A legacy entry's own meta sidecar
/// already records its cache id (`meta.model`), so migration is a
/// rename — no re-separation. A legacy dir whose new-key twin already
/// exists (the track was re-separated before migration ran) is dead
/// bytes and is removed; a dir without a parseable meta is not provably
/// ours and is left alone. Runs before every cache lookup in the worker;
/// after the first sweep it's a no-op readdir.
pub fn migrate_legacy_stem_entries(cache_dir: &Path, log: &Logger) {
    let Ok(entries) = std::fs::read_dir(cache_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()).map(String::from) else {
            continue;
        };
        if !is_legacy_key(&name) || !path.is_dir() {
            continue;
        }
        let Some(meta) = read_meta(&path) else {
            continue;
        };
        let target = cache_dir.join(format!("{name}-{}", sanitize_cache_id(&meta.model)));
        if target.exists() {
            match std::fs::remove_dir_all(&path) {
                Ok(()) => log(&format!(
                    "stem cache: removed legacy entry {name} superseded by a re-separation"
                )),
                Err(e) => log(&format!("stem cache: failed to remove legacy {name}: {e}")),
            }
        } else {
            match std::fs::rename(&path, &target) {
                Ok(()) => log(&format!(
                    "stem cache: migrated legacy entry {name} for {}",
                    meta.source_path
                )),
                // The cache is shared across worktrees/processes, so a
                // concurrent sweep can win the rename between our
                // `target.exists()` check and here (source gone, or the
                // twin landed first — rename never clobbers a non-empty
                // dir). The cache state is exactly right in both cases
                // and the next sweep is a no-op: stay silent.
                Err(_) if !path.exists() || target.exists() => {}
                Err(e) => log(&format!("stem cache: failed to migrate {name}: {e}")),
            }
        }
    }
}

fn read_meta(entry_dir: &Path) -> Option<StemCacheMeta> {
    let data = std::fs::read(entry_dir.join(META_NAME)).ok()?;
    serde_json::from_slice(&data).ok()
}

fn write_meta(entry_dir: &Path, meta: &StemCacheMeta) -> Result<(), String> {
    let data = serde_json::to_vec(meta).map_err(|e| format!("serialize stem meta: {e}"))?;
    crate::paths::atomic_write_json(&entry_dir.join(META_NAME), &data)
}

/// Look up cached stems for `source` under `cache_dir`.
///
/// A hit requires the meta sidecar to parse, its fingerprint to match the
/// source file's current `(mtime, size)`, its model to match `model`
/// (the recipe cache id), and every `layout` stem file to exist. On a
/// hit the meta is rewritten in place to bump its mtime — the LRU touch.
/// Any mismatch is a miss (the caller re-separates and [`store_stems`]
/// replaces the entry).
pub fn cached_stems(
    cache_dir: &Path,
    source: &Path,
    model: &str,
    layout: &'static [StemKind],
    log: &Logger,
) -> Option<StemSet> {
    let entry_dir = cache_dir.join(stem_cache_key(&source.to_string_lossy(), model));
    let meta = read_meta(&entry_dir)?;
    let current = FileFingerprint::from_path(source)?;
    if meta.fingerprint != current || meta.model != model {
        return None;
    }
    let set = StemSet::from_layout(&entry_dir, STEM_EXT, layout);
    if !set.all_exist() {
        log(&format!(
            "stem cache entry {} is incomplete; will re-separate",
            entry_dir.display()
        ));
        return None;
    }
    if let Err(e) = write_meta(&entry_dir, &meta) {
        // Failing to bump the LRU clock is harmless; the hit still counts.
        log(&format!("stem cache touch failed: {e}"));
    }
    Some(set)
}

/// Move freshly separated stems into the cache and prune to `max_bytes`.
///
/// The entry is staged in `{key}.tmp` with the meta sidecar written last,
/// then swapped into place — a crash mid-store leaves either the previous
/// entry or a metaless (hence invisible) stage, never a half-entry that
/// [`cached_stems`] would serve. The returned [`StemSet`] points at the
/// cached copies; `produced`'s files have been moved away.
///
/// The freshly stored entry is exempt from this prune pass so a cache cap
/// smaller than one track still plays (it gets evicted by the next store).
pub fn store_stems(
    cache_dir: &Path,
    source: &Path,
    model: &str,
    produced: &StemSet,
    max_bytes: u64,
    log: &Logger,
) -> Result<StemSet, String> {
    let fingerprint = FileFingerprint::from_path(source)
        .ok_or_else(|| format!("cannot stat source {}", source.display()))?;
    let key = stem_cache_key(&source.to_string_lossy(), model);
    let entry_dir = cache_dir.join(&key);
    let stage_dir = cache_dir.join(format!("{key}.tmp"));

    let _ = std::fs::remove_dir_all(&stage_dir);
    std::fs::create_dir_all(&stage_dir)
        .map_err(|e| format!("mkdir {} failed: {e}", stage_dir.display()))?;
    let staged = StemSet::from_layout(&stage_dir, STEM_EXT, produced.layout);
    for (src_path, dst_path) in produced.paths.iter().zip(staged.paths.iter()) {
        // rename() first (same filesystem when the worker stages under the
        // cache root); fall back to copy+remove across mount points.
        if std::fs::rename(src_path, dst_path).is_err() {
            std::fs::copy(src_path, dst_path).map_err(|e| {
                format!(
                    "move {} -> {} failed: {e}",
                    src_path.display(),
                    dst_path.display()
                )
            })?;
            let _ = std::fs::remove_file(src_path);
        }
    }
    write_meta(
        &stage_dir,
        &StemCacheMeta {
            source_path: source.to_string_lossy().into_owned(),
            fingerprint,
            model: model.to_string(),
        },
    )?;

    let _ = std::fs::remove_dir_all(&entry_dir);
    std::fs::rename(&stage_dir, &entry_dir).map_err(|e| {
        format!(
            "rename {} -> {} failed: {e}",
            stage_dir.display(),
            entry_dir.display()
        )
    })?;

    prune_stem_cache(cache_dir, max_bytes, Some(&key), log);
    Ok(StemSet::from_layout(&entry_dir, STEM_EXT, produced.layout))
}

fn dir_size_bytes(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum()
}

/// Evict least-recently-used cache entries until the total size fits in
/// `max_bytes`. "Recently used" is the meta sidecar's mtime, bumped on
/// every hit by [`cached_stems`]. `keep_key` (the just-stored entry) is
/// never evicted. Stray `.tmp` stages from crashed stores are always
/// removed. Every eviction is logged — no silent caps.
pub fn prune_stem_cache(cache_dir: &Path, max_bytes: u64, keep_key: Option<&str>, log: &Logger) {
    let Ok(entries) = std::fs::read_dir(cache_dir) else {
        return;
    };

    // (meta mtime, dir, size) for valid entries; stage dirs get cleaned.
    let mut valid: Vec<(std::time::SystemTime, PathBuf, u64)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if path.extension().is_some_and(|e| e == "tmp") {
            let _ = std::fs::remove_dir_all(&path);
            continue;
        }
        let meta_path = path.join(META_NAME);
        let Ok(meta_mtime) = std::fs::metadata(&meta_path).and_then(|m| m.modified()) else {
            // No readable meta sidecar → a crashed store or foreign dir;
            // leave it alone rather than deleting something we don't own.
            continue;
        };
        let size = dir_size_bytes(&path);
        valid.push((meta_mtime, path, size));
    }

    let mut total: u64 = valid.iter().map(|(_, _, s)| s).sum();
    if total <= max_bytes {
        return;
    }
    // Oldest first.
    valid.sort_by_key(|(mtime, _, _)| *mtime);
    for (_, path, size) in valid {
        if total <= max_bytes {
            break;
        }
        if let Some(keep) = keep_key {
            if path.file_name().is_some_and(|n| n == keep) {
                continue;
            }
        }
        match std::fs::remove_dir_all(&path) {
            Ok(()) => {
                total = total.saturating_sub(size);
                log(&format!(
                    "stem cache: evicted {} ({:.1} MB) to fit the cache cap",
                    path.display(),
                    size as f64 / (1024.0 * 1024.0)
                ));
            }
            Err(e) => log(&format!(
                "stem cache: failed to evict {}: {e}",
                path.display()
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::default_logger;

    fn temp_root(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "zytunes-stems-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    /// Write a fake source track and a produced-stems dir, returning both.
    fn fake_separation(root: &Path, body: &[u8]) -> (PathBuf, StemSet) {
        let source = root.join("song.mp3");
        std::fs::write(&source, body).unwrap();
        let produced_dir = root.join("produced");
        std::fs::create_dir_all(&produced_dir).unwrap();
        let produced = StemSet::from_layout(&produced_dir, STEM_EXT, SIX_STEM_LAYOUT);
        for p in &produced.paths {
            std::fs::write(p, b"flacdata").unwrap();
        }
        (source, produced)
    }

    #[test]
    fn recipe_parses_config_values_and_rejects_unknown() {
        for (value, kind) in [
            ("demucs", RecipeKind::Demucs),
            ("hq", RecipeKind::Hq),
            ("hq-harmony", RecipeKind::HqHarmony),
        ] {
            assert_eq!(value.parse::<RecipeKind>(), Ok(kind));
            // Display round-trips to the config value.
            assert_eq!(kind.to_string(), value);
        }
        assert!("roformer".parse::<RecipeKind>().is_err());
        assert!("".parse::<RecipeKind>().is_err());
    }

    #[test]
    fn recipe_engine_mapping() {
        use crate::stems::provision::EngineKind;
        assert_eq!(RecipeKind::Demucs.engine(), EngineKind::Demucs);
        assert_eq!(RecipeKind::Hq.engine(), EngineKind::AudioSeparator);
        assert_eq!(RecipeKind::HqHarmony.engine(), EngineKind::AudioSeparator);
    }

    #[test]
    fn recipe_cache_ids_preserve_demucs_and_version_the_rest() {
        // The demucs recipe's cache id IS the configured model string so
        // every pre-recipe cache entry (keyed "htdemucs_6s" or a custom
        // model) stays a hit after upgrading.
        assert_eq!(RecipeKind::Demucs.cache_id("htdemucs_6s"), "htdemucs_6s");
        assert_eq!(RecipeKind::Demucs.cache_id("htdemucs"), "htdemucs");
        // Multi-pass recipes carry an explicit version so a checkpoint
        // swap can force re-separation by bumping it.
        assert_eq!(RecipeKind::Hq.cache_id("htdemucs_6s"), "hq/v1");
        assert_eq!(
            RecipeKind::HqHarmony.cache_id("htdemucs_6s"),
            "hq-harmony/v1"
        );
        // Distinct recipes must never collide in the cache.
        let ids = [
            RecipeKind::Demucs.cache_id("htdemucs_6s"),
            RecipeKind::Hq.cache_id("htdemucs_6s"),
            RecipeKind::HqHarmony.cache_id("htdemucs_6s"),
        ];
        assert_eq!(
            ids.iter().collect::<std::collections::HashSet<_>>().len(),
            3
        );
    }

    #[test]
    fn stem_gains_roundtrip_and_toggle() {
        let gains = new_stem_gains(&[true, false, true, true, true, true]);
        assert_eq!(stem_gain(&gains, 0), 1.0);
        assert_eq!(stem_gain(&gains, 1), 0.0);
        set_stem_gain(&gains, 0, false);
        set_stem_gain(&gains, 1, true);
        assert_eq!(stem_gain(&gains, 0), 0.0);
        assert_eq!(stem_gain(&gains, 1), 1.0);
        // A clone shares the same atomics — the mixer's view updates.
        let mixer_view = gains.clone();
        set_stem_gain(&gains, 5, false);
        assert_eq!(stem_gain(&mixer_view, 5), 0.0);
    }

    #[test]
    fn stem_set_from_six_layout_assembles_demucs_shape() {
        let set = StemSet::from_layout(Path::new("/cache/abc"), "flac", SIX_STEM_LAYOUT);
        assert_eq!(set.paths[0], Path::new("/cache/abc/vocals.flac"));
        assert_eq!(set.paths[5], Path::new("/cache/abc/other.flac"));
    }

    #[test]
    fn build_args_shape() {
        let args = build_demucs_args(
            "htdemucs_6s",
            Path::new("/tmp/out"),
            Path::new("/music/a.flac"),
        );
        assert_eq!(
            args,
            vec![
                "-n",
                "htdemucs_6s",
                "--flac",
                "-o",
                "/tmp/out",
                "/music/a.flac"
            ]
        );
    }

    #[test]
    fn expected_output_dir_uses_model_and_track_stem() {
        let dir = expected_output_dir(
            Path::new("/tmp/out"),
            "htdemucs_6s",
            Path::new("/music/Artist/01 - Song.flac"),
        );
        assert_eq!(dir, Path::new("/tmp/out/htdemucs_6s/01 - Song"));
    }

    #[test]
    fn cache_key_embeds_recipe_and_sanitizes_slashes() {
        let a = stem_cache_key("/music/a.mp3", "htdemucs_6s");
        assert_eq!(a, stem_cache_key("/music/a.mp3", "htdemucs_6s"));
        assert_ne!(a, stem_cache_key("/music/b.mp3", "htdemucs_6s"));
        // Same track, different recipe → different entry, so recipe flips
        // don't evict each other.
        let b = stem_cache_key("/music/a.mp3", "hq/v1");
        assert_ne!(a, b);
        // Versioned cache ids carry a '/', which cannot appear in a
        // directory name.
        assert!(!b.contains('/'));
        assert!(
            a[..16].chars().all(|c| c.is_ascii_hexdigit()),
            "hash prefix stays 16-hex"
        );
        assert_eq!(&a[16..17], "-", "hash and cache id are dash-joined");
        assert!(a.ends_with("htdemucs_6s"), "cache id readable in dirname");
        // Dots are mapped away: an entry name ending in `.tmp` would be
        // indistinguishable from a stage dir and swept by the prune.
        let dotted = stem_cache_key("/music/a.mp3", "foo.tmp");
        assert!(!dotted.contains('.'), "no dots survive into the key");
        assert!(dotted.ends_with("foo-tmp"));
    }

    #[test]
    fn two_recipes_coexist_for_the_same_track() {
        let root = temp_root("coexist");
        let cache = root.join("cache");
        let log = default_logger();
        let (source, produced_demucs) = fake_separation(&root, b"mp3data");
        store_stems(
            &cache,
            &source,
            "htdemucs_6s",
            &produced_demucs,
            u64::MAX,
            &log,
        )
        .unwrap();

        // A second recipe's output for the SAME source must not evict the
        // first — flipping `[stems] recipe` back and forth for an A/B
        // must hit both ways.
        let produced_dir = root.join("produced-hq");
        std::fs::create_dir_all(&produced_dir).unwrap();
        let produced_hq = StemSet::from_layout(&produced_dir, STEM_EXT, SIX_STEM_LAYOUT);
        for p in &produced_hq.paths {
            std::fs::write(p, b"hqflac").unwrap();
        }
        store_stems(&cache, &source, "hq/v1", &produced_hq, u64::MAX, &log).unwrap();

        let demucs_hit = cached_stems(&cache, &source, "htdemucs_6s", SIX_STEM_LAYOUT, &log)
            .expect("demucs entry survives the hq store");
        let hq_hit =
            cached_stems(&cache, &source, "hq/v1", SIX_STEM_LAYOUT, &log).expect("hq entry hits");
        assert_ne!(demucs_hit.paths[0], hq_hit.paths[0]);
        assert_eq!(std::fs::read(&demucs_hit.paths[0]).unwrap(), b"flacdata");
        assert_eq!(std::fs::read(&hq_hit.paths[0]).unwrap(), b"hqflac");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn legacy_bare_hash_entry_migrates_losslessly_and_hits() {
        let root = temp_root("legacy");
        let cache = root.join("cache");
        let log = default_logger();
        let (source, _) = fake_separation(&root, b"mp3data");

        // Simulate a pre-recipe-key entry: bare 16-hex dirname holding a
        // valid meta + full six-stem file set.
        let new_key = stem_cache_key(&source.to_string_lossy(), "htdemucs_6s");
        let legacy_name = &new_key[..16];
        let legacy_dir = cache.join(legacy_name);
        std::fs::create_dir_all(&legacy_dir).unwrap();
        let legacy_set = StemSet::from_layout(&legacy_dir, STEM_EXT, SIX_STEM_LAYOUT);
        for p in &legacy_set.paths {
            std::fs::write(p, b"oldflac").unwrap();
        }
        write_meta(
            &legacy_dir,
            &StemCacheMeta {
                source_path: source.to_string_lossy().into_owned(),
                fingerprint: FileFingerprint::from_path(&source).unwrap(),
                model: "htdemucs_6s".into(),
            },
        )
        .unwrap();

        migrate_legacy_stem_entries(&cache, &log);
        assert!(!legacy_dir.exists(), "legacy dirname retired");
        assert!(cache.join(&new_key).exists(), "renamed, not re-separated");
        let hit = cached_stems(&cache, &source, "htdemucs_6s", SIX_STEM_LAYOUT, &log)
            .expect("migrated entry is a hit under the new key");
        assert_eq!(std::fs::read(&hit.paths[0]).unwrap(), b"oldflac");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn legacy_migration_leaves_foreign_dirs_and_superseded_copies() {
        let root = temp_root("legacy-edge");
        let cache = root.join("cache");
        std::fs::create_dir_all(&cache).unwrap();
        let log = default_logger();

        // A metaless 16-hex dir is not provably ours — never touched.
        let foreign = cache.join("00112233aabbccdd");
        std::fs::create_dir_all(&foreign).unwrap();

        // A legacy entry whose new-key twin already exists (the track was
        // re-separated before migration ran) is dead bytes — removed.
        let (source, produced) = fake_separation(&root, b"mp3data");
        store_stems(&cache, &source, "m", &produced, u64::MAX, &log).unwrap();
        let new_key = stem_cache_key(&source.to_string_lossy(), "m");
        let superseded = cache.join(&new_key[..16]);
        std::fs::create_dir_all(&superseded).unwrap();
        write_meta(
            &superseded,
            &StemCacheMeta {
                source_path: source.to_string_lossy().into_owned(),
                fingerprint: FileFingerprint::from_path(&source).unwrap(),
                model: "m".into(),
            },
        )
        .unwrap();

        migrate_legacy_stem_entries(&cache, &log);
        assert!(foreign.exists(), "metaless dir left alone");
        assert!(!superseded.exists(), "superseded legacy copy removed");
        assert!(cache.join(&new_key).exists(), "current entry untouched");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn store_then_hit_roundtrip() {
        let root = temp_root("roundtrip");
        let cache = root.join("cache");
        let (source, produced) = fake_separation(&root, b"mp3data");
        let log = default_logger();

        let stored = store_stems(&cache, &source, "htdemucs_6s", &produced, u64::MAX, &log)
            .expect("store succeeds");
        assert!(stored.all_exist());
        // The produced files were moved, not copied.
        assert!(!produced.paths[0].exists());

        let hit =
            cached_stems(&cache, &source, "htdemucs_6s", SIX_STEM_LAYOUT, &log).expect("cache hit");
        assert_eq!(hit, stored);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn source_change_and_model_change_invalidate() {
        let root = temp_root("invalidate");
        let cache = root.join("cache");
        let (source, produced) = fake_separation(&root, b"mp3data");
        let log = default_logger();
        store_stems(&cache, &source, "htdemucs_6s", &produced, u64::MAX, &log).unwrap();

        // Different model → miss. Under per-recipe keying this lookup
        // targets a different (nonexistent) entry dir, so it misses at
        // the key level before the meta comparison is reached.
        assert!(cached_stems(&cache, &source, "htdemucs", SIX_STEM_LAYOUT, &log).is_none());

        // Re-written source with a different size → fingerprint miss.
        std::fs::write(&source, b"mp3data-but-longer").unwrap();
        assert!(cached_stems(&cache, &source, "htdemucs_6s", SIX_STEM_LAYOUT, &log).is_none());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn sanitize_collision_still_misses_on_raw_id() {
        // "hq/v1" and "hq-v1" sanitize to the same dirname. The raw-id
        // guard in `cached_stems` (`meta.model != model`) must turn the
        // collision into a miss — thrash, never the wrong stems.
        let root = temp_root("sanitize-collision");
        let cache = root.join("cache");
        let (source, produced) = fake_separation(&root, b"mp3data");
        let log = default_logger();
        store_stems(&cache, &source, "hq/v1", &produced, u64::MAX, &log).unwrap();

        assert_eq!(
            stem_cache_key(&source.to_string_lossy(), "hq/v1"),
            stem_cache_key(&source.to_string_lossy(), "hq-v1"),
            "precondition: the two ids collide on one dirname"
        );
        assert!(
            cached_stems(&cache, &source, "hq-v1", SIX_STEM_LAYOUT, &log).is_none(),
            "same dirname, different raw id must miss"
        );
        assert!(
            cached_stems(&cache, &source, "hq/v1", SIX_STEM_LAYOUT, &log).is_some(),
            "the id that stored the entry still hits"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_stem_file_is_a_miss() {
        let root = temp_root("missing");
        let cache = root.join("cache");
        let (source, produced) = fake_separation(&root, b"mp3data");
        let log = default_logger();
        let stored =
            store_stems(&cache, &source, "htdemucs_6s", &produced, u64::MAX, &log).unwrap();

        std::fs::remove_file(&stored.paths[3]).unwrap();
        assert!(cached_stems(&cache, &source, "htdemucs_6s", SIX_STEM_LAYOUT, &log).is_none());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn prune_evicts_oldest_first_and_protects_keep_key() {
        let root = temp_root("prune");
        let cache = root.join("cache");
        std::fs::create_dir_all(&cache).unwrap();
        let log = default_logger();

        // Three entries, 8 bytes of stem data each (6 files + meta).
        // Meta mtimes are staggered by explicit rewrites with sleeps —
        // coarse (1 s) filesystem timestamps would make ordering flaky,
        // so use set-order-friendly distinct payload writes plus
        // filetime-free spacing via sleeping over the mtime granularity.
        let mut keys = Vec::new();
        for name in ["a.mp3", "b.mp3", "c.mp3"] {
            let (source, produced) = fake_separation(&temp_root(name), b"x");
            store_stems(&cache, &source, "m", &produced, u64::MAX, &log).unwrap();
            keys.push(stem_cache_key(&source.to_string_lossy(), "m"));
            std::thread::sleep(std::time::Duration::from_millis(1100));
        }

        let entry_size = dir_size_bytes(&cache.join(&keys[0]));
        assert!(entry_size > 0);

        // Cap fits exactly two entries → the oldest (a) must go.
        prune_stem_cache(&cache, entry_size * 2, None, &log);
        assert!(!cache.join(&keys[0]).exists(), "oldest evicted");
        assert!(cache.join(&keys[1]).exists());
        assert!(cache.join(&keys[2]).exists());

        // Cap fits one entry but the oldest survivor is protected by
        // keep_key → the other one goes instead.
        prune_stem_cache(&cache, entry_size, Some(&keys[1]), &log);
        assert!(cache.join(&keys[1]).exists(), "keep_key protected");
        assert!(!cache.join(&keys[2]).exists());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn pinned_model_files_names_every_recipe_checkpoint() {
        let pinned = pinned_model_files();
        for name in [
            AUDIO_SEPARATOR_HTDEMUCS_MODEL,
            BS_ROFORMER_VOCALS_MODEL,
            MEL_ROFORMER_KARAOKE_MODEL,
        ] {
            assert!(pinned.contains(&name), "{name} must be in the pinned set");
        }
    }

    #[test]
    fn prune_model_cache_removes_retired_ckpts_and_their_sidecars() {
        let root = temp_root("model-prune");
        let dir = root.join("models");
        std::fs::create_dir_all(&dir).unwrap();
        // Currently pinned checkpoint + its engine-downloaded config.
        std::fs::write(dir.join(BS_ROFORMER_VOCALS_MODEL), b"pinned").unwrap();
        let pinned_yaml = format!(
            "{}.yaml",
            Path::new(BS_ROFORMER_VOCALS_MODEL)
                .file_stem()
                .unwrap()
                .to_str()
                .unwrap()
        );
        std::fs::write(dir.join(&pinned_yaml), b"cfg").unwrap();
        // A checkpoint retired by a pin bump, plus its sidecar config —
        // and a same-stem engine-managed file outside the sidecar
        // allowlist, which must survive the sweep.
        std::fs::write(dir.join("old_roformer_sdr_1.0.ckpt"), b"retired").unwrap();
        std::fs::write(dir.join("old_roformer_sdr_1.0.yaml"), b"cfg").unwrap();
        std::fs::write(dir.join("old_roformer_sdr_1.0.th"), b"weights").unwrap();
        // Engine-managed files whose names we don't control — never touched.
        std::fs::write(dir.join("5c90dfd2-34c22ccb.th"), b"demucs-weights").unwrap();
        std::fs::write(dir.join("model-data.json"), b"registry").unwrap();

        prune_model_cache(&dir, &pinned_model_files(), &default_logger());

        assert!(dir.join(BS_ROFORMER_VOCALS_MODEL).exists(), "pinned stays");
        assert!(dir.join(&pinned_yaml).exists(), "pinned sidecar stays");
        assert!(
            !dir.join("old_roformer_sdr_1.0.ckpt").exists(),
            "retired checkpoint evicted"
        );
        assert!(
            !dir.join("old_roformer_sdr_1.0.yaml").exists(),
            "retired sidecar evicted with it"
        );
        assert!(
            dir.join("old_roformer_sdr_1.0.th").exists(),
            "same-stem file outside the sidecar allowlist stays"
        );
        assert!(dir.join("5c90dfd2-34c22ccb.th").exists(), ".th untouched");
        assert!(dir.join("model-data.json").exists(), "loose json untouched");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn prune_model_cache_never_retires_a_pinned_stem() {
        // AUDIO_SEPARATOR_HTDEMUCS_MODEL is pinned by its *yaml* name. A
        // stray non-pinned same-stem .ckpt must not mark that stem
        // retired — the sweep would otherwise take the live model's
        // sidecars with it, forcing a silent re-download. Leaking the
        // stray file is the safe outcome.
        let root = temp_root("model-prune-pinned-stem");
        let dir = root.join("models");
        std::fs::create_dir_all(&dir).unwrap();
        let stem = Path::new(AUDIO_SEPARATOR_HTDEMUCS_MODEL)
            .file_stem()
            .unwrap()
            .to_str()
            .unwrap();
        std::fs::write(dir.join(AUDIO_SEPARATOR_HTDEMUCS_MODEL), b"pinned").unwrap();
        std::fs::write(dir.join(format!("{stem}.ckpt")), b"stray").unwrap();
        std::fs::write(dir.join(format!("{stem}.json")), b"engine-cfg").unwrap();

        prune_model_cache(&dir, &pinned_model_files(), &default_logger());

        assert!(
            dir.join(AUDIO_SEPARATOR_HTDEMUCS_MODEL).exists(),
            "pinned yaml stays"
        );
        assert!(
            dir.join(format!("{stem}.ckpt")).exists(),
            "stray same-stem ckpt is leaked, not retired"
        );
        assert!(
            dir.join(format!("{stem}.json")).exists(),
            "live model's sidecar survives"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn prune_model_cache_tolerates_missing_dir() {
        // No models dir yet (demucs-only user) — must be a silent no-op.
        prune_model_cache(
            Path::new("/nonexistent/zytunes-models"),
            &pinned_model_files(),
            &default_logger(),
        );
    }

    #[test]
    fn prune_cleans_stale_stage_dirs() {
        let root = temp_root("stage");
        let cache = root.join("cache");
        let stale = cache.join("deadbeef.tmp");
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::write(stale.join("vocals.flac"), b"partial").unwrap();

        prune_stem_cache(&cache, u64::MAX, None, &default_logger());
        assert!(!stale.exists(), "crashed-store stage removed");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn store_replaces_existing_entry() {
        let root = temp_root("replace");
        let cache = root.join("cache");
        let log = default_logger();
        let (source, produced) = fake_separation(&root, b"v1");
        store_stems(&cache, &source, "m", &produced, u64::MAX, &log).unwrap();

        // Source re-tagged → new fingerprint, fresh separation, re-store.
        std::fs::write(&source, b"v2-longer").unwrap();
        let produced_dir = root.join("produced2");
        std::fs::create_dir_all(&produced_dir).unwrap();
        let produced2 = StemSet::from_layout(&produced_dir, STEM_EXT, SIX_STEM_LAYOUT);
        for p in &produced2.paths {
            std::fs::write(p, b"newflac").unwrap();
        }
        let stored = store_stems(&cache, &source, "m", &produced2, u64::MAX, &log).unwrap();
        assert!(stored.all_exist());
        assert_eq!(std::fs::read(&stored.paths[0]).unwrap(), b"newflac");

        let hit =
            cached_stems(&cache, &source, "m", SIX_STEM_LAYOUT, &log).expect("fresh entry hits");
        assert_eq!(hit, stored);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn audio_separator_args_shape_and_output_names_json() {
        let names = [("Vocals", "vocals"), ("Drums", "drums")];
        let args = build_audio_separator_args(
            "htdemucs_6s.yaml",
            Path::new("/tmp/out"),
            Path::new("/cache/models"),
            &names,
            Path::new("/music/a.flac"),
        );
        assert_eq!(args[0], "/music/a.flac", "input is positional and first");
        for (flag, value) in [
            ("-m", "htdemucs_6s.yaml"),
            ("--output_dir", "/tmp/out"),
            ("--output_format", "flac"),
            ("--model_file_dir", "/cache/models"),
        ] {
            assert!(
                args.windows(2).any(|w| w[0] == flag && w[1] == value),
                "expected `{flag} {value}` in {args:?}"
            );
        }
        // The output-name mapping must survive the JSON round trip; the
        // exact key strings are the engine's stem names for the model.
        let idx = args
            .iter()
            .position(|a| a == "--custom_output_names")
            .expect("--custom_output_names present");
        let parsed: serde_json::Value = serde_json::from_str(&args[idx + 1]).unwrap();
        assert_eq!(parsed["Vocals"], "vocals");
        assert_eq!(parsed["Drums"], "drums");
        assert_eq!(parsed.as_object().unwrap().len(), 2);
    }

    #[test]
    fn stem_layouts_shape() {
        assert_eq!(SIX_STEM_LAYOUT.len(), 6);
        assert_eq!(SIX_STEM_LAYOUT[0], StemKind::Vocals);
        assert_eq!(
            HARMONY_STEM_LAYOUT,
            &[
                StemKind::LeadVocals,
                StemKind::BackingVocals,
                StemKind::Drums,
                StemKind::Bass,
                StemKind::Guitar,
                StemKind::Piano,
                StemKind::Other,
            ]
        );
        assert!(SIX_STEM_LAYOUT.len() <= MAX_STEMS);
        assert!(HARMONY_STEM_LAYOUT.len() <= MAX_STEMS);

        assert_eq!(RecipeKind::Demucs.layout(), SIX_STEM_LAYOUT);
        assert_eq!(RecipeKind::Hq.layout(), SIX_STEM_LAYOUT);
        assert_eq!(RecipeKind::HqHarmony.layout(), HARMONY_STEM_LAYOUT);

        // The strip renderer assumes exactly-3-char short labels and
        // unique file stems for every kind in every layout.
        let mut seen = std::collections::HashSet::new();
        for kind in HARMONY_STEM_LAYOUT.iter().chain(SIX_STEM_LAYOUT) {
            assert_eq!(kind.short_label().chars().count(), 3, "{kind:?}");
            seen.insert(kind.file_stem());
        }
        assert_eq!(seen.len(), 8, "8 distinct kinds across both layouts");
    }

    #[test]
    fn harmony_passes_produce_exactly_the_harmony_layout() {
        let passes = hq_harmony_recipe_passes();
        assert_eq!(passes.len(), 3, "vocals, band, karaoke");

        // Pass 1 extracts the full vocal take to an INTERMEDIATE name —
        // the harmony layout has no plain "vocals" stem to collide with.
        assert!(matches!(passes[0].input, PassInput::Source));
        assert!(passes[0]
            .output_names
            .iter()
            .any(|(_, v)| *v == "vocals_full"));

        // Pass 3 splits that take into lead + backing with the pinned
        // karaoke checkpoint.
        assert!(matches!(
            passes[2].input,
            PassInput::Produced("vocals_full")
        ));
        assert_eq!(passes[2].model, MEL_ROFORMER_KARAOKE_MODEL);

        // Union of outputs covers the harmony layout exactly once.
        for kind in HARMONY_STEM_LAYOUT {
            let count = passes
                .iter()
                .flat_map(|p| p.output_names.iter())
                .filter(|(_, v)| *v == kind.file_stem())
                .count();
            assert_eq!(count, 1, "{kind:?} must be produced exactly once");
        }

        // Windows tile 0..=100.
        assert_eq!(passes[0].window.0, 0);
        assert_eq!(passes[0].window.1, passes[1].window.0);
        assert_eq!(passes[1].window.1, passes[2].window.0);
        assert_eq!(passes[2].window.1, 100);
    }

    #[test]
    fn stem_set_from_layout_orders_paths() {
        let set = StemSet::from_layout(Path::new("/cache/abc"), "flac", HARMONY_STEM_LAYOUT);
        assert_eq!(set.paths.len(), 7);
        assert_eq!(set.paths[0], Path::new("/cache/abc/lead.flac"));
        assert_eq!(set.paths[1], Path::new("/cache/abc/backing.flac"));
        assert_eq!(set.paths[6], Path::new("/cache/abc/other.flac"));
        assert_eq!(set.layout, HARMONY_STEM_LAYOUT);
    }

    #[test]
    fn stem_gains_pad_to_capacity() {
        // A 7-stem layout fills 7 slots; the unused capacity idles muted.
        let gains = new_stem_gains(&[true; 7]);
        for i in 0..7 {
            assert_eq!(stem_gain(&gains, i), 1.0);
        }
        assert_eq!(stem_gain(&gains, 7), 0.0, "unused slot stays silent");
    }

    #[test]
    fn store_then_hit_roundtrip_with_harmony_layout() {
        let root = temp_root("harmony-roundtrip");
        let cache = root.join("cache");
        let source = root.join("song.mp3");
        std::fs::write(&source, b"mp3data").unwrap();
        let produced_dir = root.join("produced");
        std::fs::create_dir_all(&produced_dir).unwrap();
        let produced = StemSet::from_layout(&produced_dir, STEM_EXT, HARMONY_STEM_LAYOUT);
        for p in &produced.paths {
            std::fs::write(p, b"flacdata").unwrap();
        }
        let log = default_logger();

        let stored = store_stems(&cache, &source, "hq-harmony/v1", &produced, u64::MAX, &log)
            .expect("store succeeds");
        assert_eq!(stored.paths.len(), 7);
        assert!(stored.all_exist());

        let hit = cached_stems(&cache, &source, "hq-harmony/v1", HARMONY_STEM_LAYOUT, &log)
            .expect("cache hit");
        assert_eq!(hit, stored);

        // The same entry looked up under the six-stem layout misses:
        // there is no vocals.flac in a harmony entry.
        assert!(cached_stems(&cache, &source, "hq-harmony/v1", SIX_STEM_LAYOUT, &log).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn hq_passes_produce_exactly_the_six_canonical_stems() {
        let passes = hq_recipe_passes();
        assert_eq!(passes.len(), 2, "vocals pass + band pass");

        // Pass 1: BS-Roformer over the source, producing the vocals stem
        // and the intermediate the band pass consumes.
        assert!(matches!(passes[0].input, PassInput::Source));
        assert_eq!(passes[0].model, BS_ROFORMER_VOCALS_MODEL);
        assert!(passes[0]
            .output_names
            .iter()
            .any(|(k, v)| *k == "Vocals" && *v == "vocals"));
        assert!(passes[0]
            .output_names
            .iter()
            .any(|(_, v)| *v == "instrumental"));

        // Pass 2: htdemucs_6s over pass 1's instrumental.
        assert!(matches!(
            passes[1].input,
            PassInput::Produced("instrumental")
        ));
        assert_eq!(passes[1].model, AUDIO_SEPARATOR_HTDEMUCS_MODEL);
        // Its vocals output is residue (the input is already devocalized)
        // and must NOT overwrite pass 1's real vocals stem.
        let band_vocals = passes[1]
            .output_names
            .iter()
            .find(|(k, _)| *k == "Vocals")
            .expect("pass 2 must name its vocals output somewhere");
        assert_ne!(
            band_vocals.1, "vocals",
            "residue may not clobber the real stem"
        );

        // Union of all pass outputs covers every canonical stem exactly once.
        for kind in SIX_STEM_LAYOUT {
            let count = passes
                .iter()
                .flat_map(|p| p.output_names.iter())
                .filter(|(_, v)| *v == kind.file_stem())
                .count();
            assert_eq!(count, 1, "{kind:?} must be produced exactly once");
        }

        // Progress windows tile 0..=100 without gaps or overlap.
        assert_eq!(passes[0].window.0, 0);
        assert_eq!(passes[0].window.1, passes[1].window.0);
        assert_eq!(passes[1].window.1, 100);
    }

    /// A stub `audio-separator` that honors `--output_dir` and
    /// `--custom_output_names`: it creates one file per mapped output
    /// name, so multi-pass plumbing (intermediates feeding later passes)
    /// is exercised for real, engine-free.
    #[cfg(unix)]
    fn write_stub_audio_separator(dir: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let script = dir.join("audio-separator");
        std::fs::write(
            &script,
            "#!/bin/sh\n\
             out=\"\"; json=\"\"; prev=\"\"\n\
             for a in \"$@\"; do\n\
               case \"$prev\" in\n\
                 --output_dir) out=\"$a\";;\n\
                 --custom_output_names) json=\"$a\";;\n\
               esac\n\
               prev=\"$a\"\n\
             done\n\
             [ -n \"$out\" ] || exit 9\n\
             mkdir -p \"$out\"\n\
             echo \"$json\" | tr '{,}' '\\n\\n\\n' | sed -n 's/.*:\"\\([^\"]*\\)\".*/\\1/p' | \\\n\
             while read -r f; do\n\
               : > \"$out/$f.flac\"\n\
             done\n\
             printf ' 50%%|#####     |\\r' 1>&2\n\
             echo done-stub\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    #[test]
    #[cfg(unix)]
    fn cascade_runs_all_passes_and_windows_progress() {
        let root = temp_root("cascade");
        let src = root.join("song.flac");
        std::fs::write(&src, b"flac").unwrap();
        let out_dir = root.join("work");
        let sep = CascadeSeparator {
            engine: AudioSeparatorCli {
                command: write_stub_audio_separator(&root),
                model_file_dir: root.join("models"),
            },
            passes: hq_recipe_passes(),
            layout: SIX_STEM_LAYOUT,
        };
        let ticks = std::sync::Mutex::new(Vec::new());
        let lines = std::sync::Mutex::new(Vec::new());
        let set = sep
            .separate(
                &src,
                &out_dir,
                &|| false,
                &|p| ticks.lock().unwrap().push(p),
                &|l: &str| lines.lock().unwrap().push(l.to_string()),
            )
            .expect("stub cascade succeeds");
        assert!(set.all_exist());
        assert_eq!(set.paths[0], out_dir.join("vocals.flac"));

        // The stub emits a 50% tick per pass: windowed into (0,45) that
        // is 22, into (45,100) it is 72. Ticks must be monotone within
        // the run and end at 100.
        let ticks = ticks.into_inner().unwrap();
        assert!(ticks.contains(&22), "pass-1 windowed tick: {ticks:?}");
        assert!(ticks.contains(&72), "pass-2 windowed tick: {ticks:?}");
        assert_eq!(*ticks.last().unwrap(), 100);
        assert!(
            ticks.windows(2).all(|w| w[0] <= w[1]),
            "monotone: {ticks:?}"
        );

        // Each pass announces itself in the log.
        let lines = lines.into_inner().unwrap();
        assert!(lines.iter().any(|l| l.contains("pass 1/2")), "{lines:?}");
        assert!(lines.iter().any(|l| l.contains("pass 2/2")), "{lines:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    #[cfg(unix)]
    fn cascade_missing_intermediate_is_missing_output_error() {
        use std::os::unix::fs::PermissionsExt;
        let root = temp_root("cascade-missing");
        let src = root.join("song.flac");
        std::fs::write(&src, b"flac").unwrap();
        // Stub writes nothing: pass 1 "succeeds" but produces no
        // instrumental, so pass 2's input is missing — the error must
        // name that file, not a generic engine failure.
        let script = root.join("audio-separator");
        std::fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let sep = CascadeSeparator {
            engine: AudioSeparatorCli {
                command: script,
                model_file_dir: root.join("models"),
            },
            passes: hq_recipe_passes(),
            layout: SIX_STEM_LAYOUT,
        };
        let err = sep
            .separate(&src, &root.join("work"), &|| false, &|_| {}, &|_| {})
            .unwrap_err();
        match err {
            StemError::MissingOutput(p) => {
                assert!(
                    p.to_string_lossy().contains("instrumental"),
                    "should name the missing intermediate: {p:?}"
                )
            }
            other => panic!("expected MissingOutput, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn audio_separator_available_reports_missing_binary() {
        let sep = AudioSeparatorCli {
            command: PathBuf::from("/nonexistent/zytunes-audiosep-test"),
            model_file_dir: PathBuf::from("/tmp"),
        };
        let err = sep.available().unwrap_err();
        assert!(err.contains("not found"), "{err}");
    }

    /// Live-engine coverage (spawn/cancel/kill) requires a real demucs
    /// install, so it lives in the manual workflow; `available()` on a
    /// nonexistent binary is the one subprocess path testable everywhere.
    #[test]
    fn available_reports_missing_binary() {
        let sep = DemucsCli {
            command: PathBuf::from("/nonexistent/zytunes-demucs-test"),
            model: "htdemucs_6s".into(),
            layout: SIX_STEM_LAYOUT,
        };
        let err = sep.available().unwrap_err();
        assert!(err.contains("not found"), "{err}");
    }
}
