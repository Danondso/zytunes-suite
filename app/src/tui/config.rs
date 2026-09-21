use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Default)]
pub struct Config {
    pub theme: Option<String>,
    pub music_dir: Option<String>,
    pub photo_dir: Option<String>,
    pub video_dir: Option<String>,
    /// Shared cache root (dirlib scans, album art, default stem cache,
    /// model checkpoints, play history). Unset uses `~/.cache/zytunes`.
    /// A blank string reads as unset. Does not relocate device-scoped
    /// caches (`ZYTUNES_CACHE_DIR` still does that).
    pub cache_dir: Option<String>,
    /// Path to the MTPZ handshake file (5 hex lines). Unset uses
    /// `~/.mtpz-data`. `ZYTUNES_MTPZ_DATA` wins over this field. A blank
    /// string reads as unset. iPod sync does not use this file.
    pub mtpz_data: Option<String>,
    pub album_art_style: Option<String>,
    /// Now-playing soundbar draw style (`meters` / `eq` / `mirror` /
    /// `pulse` / `dots`). Unset follows the active theme. `W` writes an
    /// override; picking a theme clears it.
    pub soundbar_style: Option<String>,
    /// User preference for the now-playing panel. `None` means "auto" (show
    /// when there's a track and the terminal is tall enough). `Some(false)`
    /// force-hides the panel even when playback is active.
    pub show_player: Option<bool>,
    /// Compute Chromaprint acoustic fingerprints during the library scan.
    /// `None` (default) and `Some(true)` enable; `Some(false)` skips the
    /// expensive symphonia + chromaprint pass entirely. When disabled the
    /// scan is fast (lofty tag read only) but Phase 2+ playcount-merge
    /// features that key on `acoustic_id` won't have anything to match on.
    pub fingerprinting: Option<bool>,
    /// Compute and embed a Chromaprint fingerprint into each freshly-ripped
    /// CD track as an `ACOUSTID_FINGERPRINT` tag. `None` (default) and
    /// `Some(true)` enable; `Some(false)` skips it. Decoupled from
    /// `fingerprinting` (scan-time) so a user can opt out of rip-time
    /// fingerprinting without losing scan-time identity matching. Adds
    /// ~1–15 s per track during rip (capped at 120 s of audio decode).
    pub acoustid_fingerprint: Option<bool>,
    /// MusicBrainz Web Service base URL. `None` uses the public host
    /// (`https://musicbrainz.org/ws/2`). Point at a locally hosted mirror
    /// (e.g. `http://localhost:5000/ws/2`) to skip rate limits.
    pub musicbrainz_base_url: Option<String>,
    /// User-Agent string sent on every MusicBrainz request. Required by the
    /// public host per MB Terms of Service. Format:
    /// `application/version (contact)` — e.g.
    /// `zytunes/2.2.0 (you@example.com)`.
    pub musicbrainz_user_agent: Option<String>,
    /// AcoustID application API key — required by api.acoustid.org for the
    /// fingerprint → MBID lookup the tag-manager uses as a fallback when
    /// the library has no MBID and search-by-text isn't available.
    /// Register one for free at <https://acoustid.org/new-application>;
    /// without a key the tag-manager skips AcoustID dispatch silently and
    /// falls through to MB search.
    pub acoustid_app_key: Option<String>,
    /// Default rip fidelity preselected in the CD import overlay.
    /// Accepts `mp3-cbr-320` (alias `mp3-320`), `mp3-v0`, `mp3-v2`,
    /// `flac`, `wav`. Unset / unparseable defaults to FLAC (lossless
    /// archival).
    pub default_fidelity: Option<String>,
    /// MP3 encoder target for device-side transcode (Zune always; iPod
    /// when the source is not FLAC→ALAC). Accepts `v0`, `v2` (default),
    /// `v4`, `cbr-128`, `cbr-192`, `cbr-256`, `cbr-320` (and aliases
    /// `mp3-v0` / `320`). Unset / unparseable falls back to v2 with a
    /// warning. Override: `--quality` / `ZYTUNES_TRANSCODE_QUALITY`.
    pub transcode_quality: Option<String>,
    /// Default auto-eject preference for the CD import overlay. The
    /// overlay's own checkbox can override per-import. Unset defaults
    /// to `true` so a successful rip ejects the disc.
    pub cd_auto_eject: Option<bool>,
    /// User-defined custom themes keyed by theme name. Each entry inherits
    /// missing fields from its `base` (or `iTunes 2004` when unset) and merges
    /// into the theme picker alongside the built-ins.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub themes: BTreeMap<String, UserTheme>,
    /// `[stems]` table — stem-separation engine and cache settings.
    #[serde(default, skip_serializing_if = "StemsConfig::is_default")]
    pub stems: StemsConfig,
}

/// `[stems]` — settings for stem-split playback (`M` in the player).
/// Everything is optional with working defaults; see the accessors for
/// the effective values.
#[derive(Serialize, Deserialize, Default, Clone, PartialEq, Debug)]
pub struct StemsConfig {
    /// Engine provisioning strategy: `"auto"` (default — offer a
    /// uv-managed install when no engine is found) or `"manual"` (never
    /// install; report the missing engine instead). `"bundled"` is
    /// reserved for a future pre-built distribution.
    pub provision: Option<String>,
    /// Explicit engine binary path (venv/pipx/system). Set by hand for
    /// manual installs; the auto-provisioner writes the resolved path
    /// back here after a successful install. Shared across engines, so
    /// discovery only honors it for the engine whose exe name the file
    /// name contains (`demucs` vs `audio-separator`) — a path left
    /// behind by the other engine's install falls through to PATH / the
    /// managed dir instead of being driven with the wrong argv.
    pub command: Option<String>,
    /// pip requirement spec the auto-provisioner installs. Swap for a
    /// maintained fork (e.g. `demucs-next`) without a zytunes release.
    /// Applies only to the engine whose package family it names (same
    /// prefix rule as `command`); the other engine keeps its default.
    pub package: Option<String>,
    /// Demucs model name (demucs recipe only). Participates in stem-cache
    /// validity via the entry's recorded model id — changing it
    /// re-separates on next use.
    pub model: Option<String>,
    /// `true` installs the default (CUDA/MPS-capable) torch instead of
    /// the much smaller CPU-only build. Auto-provision only.
    pub gpu: Option<bool>,
    /// Stem cache size cap in GiB (LRU-pruned).
    pub cache_max_gb: Option<u64>,
    /// Override for the stem-cache directory. Unset (default) keeps the
    /// cache under `~/.cache/zytunes/stems`. Point it at another location
    /// to relocate the (large, per-track FLAC) cache onto a roomier disk,
    /// or somewhere easy to reach when you want to grab the separated
    /// stems by hand. A blank string behaves like unset.
    pub cache_dir: Option<String>,
    /// Separation recipe: `"demucs"` (default), `"hq"` (the Roformer
    /// vocals / demucs band cascade), `"sw"` (single-pass 6-stem
    /// BS-RoFormer-SW), or `"hq-harmony"` (adds lead/backing vocal
    /// stems). Its `cache_id` is recorded in each
    /// stem-cache entry's metadata, so switching recipes re-separates on
    /// next use (the entry directory itself is keyed by source path
    /// alone — see todos.md for per-recipe coexistence).
    pub recipe: Option<String>,
}

impl StemsConfig {
    fn is_default(&self) -> bool {
        *self == StemsConfig::default()
    }

    /// Auto-provisioning is on unless `provision = "manual"`.
    pub fn auto_provision(&self) -> bool {
        self.provision.as_deref() != Some("manual")
    }

    pub fn model(&self) -> String {
        self.model
            .clone()
            .unwrap_or_else(|| "htdemucs_6s".to_string())
    }

    pub fn gpu(&self) -> bool {
        self.gpu.unwrap_or(false)
    }

    pub fn cache_max_bytes(&self) -> u64 {
        self.cache_max_gb.unwrap_or(10).saturating_mul(1 << 30)
    }

    /// Effective stem-cache root: the `cache_dir` override when set and
    /// non-empty, else the default `~/.cache/zytunes/stems`. `None` only
    /// when no override is set *and* `$HOME` can't be resolved.
    pub fn stem_cache_dir(&self) -> Option<std::path::PathBuf> {
        self.cache_dir
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(std::path::PathBuf::from)
            .or_else(zytunes::stems::default_stem_cache_dir)
    }

    /// Parsed `recipe`, blank/unset defaulting to demucs. `Err` carries
    /// the raw unknown value so the caller can log it before falling
    /// back — a typo'd recipe silently downgrading to demucs quality
    /// would be a confusing failure mode.
    pub fn recipe_kind(&self) -> Result<zytunes::stems::RecipeKind, String> {
        match self.recipe.as_deref().map(str::trim) {
            None | Some("") => Ok(zytunes::stems::RecipeKind::Demucs),
            Some(raw) => raw.parse(),
        }
    }

    /// Package spec the provisioner installs for `engine`: the explicit
    /// `package` override when it names a package in `engine`'s family
    /// (`demucs-next` → Demucs, `audio-separator[gpu]==X` →
    /// AudioSeparator), else the engine's default (which for
    /// audio-separator honors the `gpu` flag's extra). A demucs-era
    /// override must not leak into an audio-separator install: uv would
    /// install the wrong package "successfully" and provisioning would
    /// then dead-end on the missing engine binary.
    pub fn resolved_package(&self, engine: zytunes::stems::provision::EngineKind) -> String {
        use zytunes::stems::provision::package_name;
        self.package
            .clone()
            .filter(|spec| {
                package_name(spec).starts_with(package_name(&engine.default_package(false)))
            })
            .unwrap_or_else(|| engine.default_package(self.gpu()))
    }

    /// Explicit engine path, ignoring empty strings so a commented-out
    /// or blanked field behaves like unset.
    pub fn command_path(&self) -> Option<std::path::PathBuf> {
        self.command
            .as_deref()
            .filter(|c| !c.trim().is_empty())
            .map(std::path::PathBuf::from)
    }
}

/// Configurable overrides for a custom theme. All fields are optional and
/// fall back to the named `base` theme's value when absent.
#[derive(Serialize, Deserialize, Default, Clone, Debug)]
pub struct UserTheme {
    pub base: Option<String>,
    pub sidebar_bg: Option<String>,
    pub sidebar_text: Option<String>,
    pub selection_bg: Option<String>,
    pub selection_text: Option<String>,
    pub main_bg: Option<String>,
    pub alt_row_bg: Option<String>,
    pub border: Option<String>,
    pub footer_bg: Option<String>,
    pub footer_text: Option<String>,
    pub header_text: Option<String>,
    pub dim_text: Option<String>,
    pub error_text: Option<String>,
    pub success_text: Option<String>,
    pub progress_bar: Option<String>,
    pub progress_bg: Option<String>,
    pub accent_secondary: Option<String>,
    pub border_type: Option<String>,
    pub header_modifier: Option<String>,
    pub sidebar_modifier: Option<String>,
    pub dim_modifier: Option<String>,
    pub footer_modifier: Option<String>,
    pub accent_anim: Option<String>,
}

fn config_dir() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(".config").join("zytunes"))
}

fn config_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("config.toml"))
}

pub fn load() -> Config {
    let path = match config_path() {
        Some(p) => p,
        None => return Config::default(),
    };
    let contents = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return Config::default(),
    };
    toml::from_str(&contents).unwrap_or_default()
}

/// Safely read-modify-write the on-disk config.
///
/// If the file exists but fails to parse, `update` returns without writing —
/// this prevents a stray keypress from clobbering other fields the user had set.
/// If the file is merely absent, a new file is written from defaults.
pub fn update(f: impl FnOnce(&mut Config)) {
    let dir = match config_dir() {
        Some(d) => d,
        None => return,
    };
    let path = dir.join("config.toml");
    if let Some(new_contents) = update_contents(std::fs::read_to_string(&path).ok().as_deref(), f) {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(&path, new_contents);
    }
}

/// Pure core of [`update`]: takes the existing file contents (if any) and a mutator,
/// and returns the serialized new contents to write — or `None` if the existing
/// contents are present but unparseable (in which case the caller should not write).
fn update_contents(existing: Option<&str>, f: impl FnOnce(&mut Config)) -> Option<String> {
    let mut config = match existing {
        Some(contents) => toml::from_str::<Config>(contents).ok()?,
        None => Config::default(),
    };
    f(&mut config);
    toml::to_string_pretty(&config).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_backwards_compatible_without_new_fields() {
        let toml_str = r#"
theme = "Gruvbox Dark"
music_dir = "/home/user/Music"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.theme.as_deref(), Some("Gruvbox Dark"));
        assert_eq!(config.music_dir.as_deref(), Some("/home/user/Music"));
        assert!(config.photo_dir.is_none());
        assert!(config.video_dir.is_none());
        assert!(config.cache_dir.is_none());
        assert!(config.mtpz_data.is_none());
        assert!(config.album_art_style.is_none());
        assert!(config.soundbar_style.is_none());
        assert!(config.show_player.is_none());
    }

    #[test]
    fn config_round_trip_show_player() {
        for pref in [Some(true), Some(false), None] {
            let config = Config {
                show_player: pref,
                ..Config::default()
            };
            let serialized = toml::to_string_pretty(&config).unwrap();
            let deserialized: Config = toml::from_str(&serialized).unwrap();
            assert_eq!(deserialized.show_player, pref);
        }
    }

    #[test]
    fn config_round_trip_with_new_fields() {
        let config = Config {
            theme: Some("BIOS".into()),
            music_dir: Some("/music".into()),
            photo_dir: Some("/photos".into()),
            video_dir: Some("/videos".into()),
            album_art_style: Some("ascii".into()),
            show_player: Some(true),
            fingerprinting: Some(false),
            ..Config::default()
        };
        let serialized = toml::to_string_pretty(&config).unwrap();
        let deserialized: Config = toml::from_str(&serialized).unwrap();
        assert_eq!(deserialized.photo_dir.as_deref(), Some("/photos"));
        assert_eq!(deserialized.video_dir.as_deref(), Some("/videos"));
        assert_eq!(deserialized.album_art_style.as_deref(), Some("ascii"));
        assert_eq!(deserialized.show_player, Some(true));
        assert_eq!(deserialized.fingerprinting, Some(false));
    }

    #[test]
    fn config_default_all_none() {
        let config = Config::default();
        assert!(config.theme.is_none());
        assert!(config.music_dir.is_none());
        assert!(config.photo_dir.is_none());
        assert!(config.video_dir.is_none());
        assert!(config.cache_dir.is_none());
        assert!(config.mtpz_data.is_none());
        assert!(config.album_art_style.is_none());
        assert!(config.soundbar_style.is_none());
        assert!(config.show_player.is_none());
        assert!(config.fingerprinting.is_none());
        assert!(config.acoustid_fingerprint.is_none());
        assert!(config.musicbrainz_base_url.is_none());
        assert!(config.musicbrainz_user_agent.is_none());
        assert!(config.default_fidelity.is_none());
        assert!(config.transcode_quality.is_none());
        assert!(config.cd_auto_eject.is_none());
        assert!(config.themes.is_empty());
    }

    #[test]
    fn config_round_trip_acoustid_fingerprint() {
        for pref in [Some(true), Some(false), None] {
            let config = Config {
                acoustid_fingerprint: pref,
                ..Config::default()
            };
            let serialized = toml::to_string_pretty(&config).unwrap();
            let deserialized: Config = toml::from_str(&serialized).unwrap();
            assert_eq!(deserialized.acoustid_fingerprint, pref);
        }
    }

    #[test]
    fn config_round_trip_fingerprinting() {
        for pref in [Some(true), Some(false), None] {
            let config = Config {
                fingerprinting: pref,
                ..Config::default()
            };
            let serialized = toml::to_string_pretty(&config).unwrap();
            let deserialized: Config = toml::from_str(&serialized).unwrap();
            assert_eq!(deserialized.fingerprinting, pref);
        }
    }

    #[test]
    fn config_parses_user_themes_table() {
        let toml_str = r##"
theme = "My Custom"

[themes."My Custom"]
base = "Gruvbox Dark"
selection_bg = "#ff00aa"
progress_bar = "#00ffaa"
accent_anim = "pulse"
"##;
        let config: Config = toml::from_str(toml_str).unwrap();
        let ut = config.themes.get("My Custom").expect("theme present");
        assert_eq!(ut.base.as_deref(), Some("Gruvbox Dark"));
        assert_eq!(ut.selection_bg.as_deref(), Some("#ff00aa"));
        assert_eq!(ut.progress_bar.as_deref(), Some("#00ffaa"));
        assert_eq!(ut.accent_anim.as_deref(), Some("pulse"));
    }

    #[test]
    fn empty_themes_table_is_not_serialized() {
        let config = Config::default();
        let serialized = toml::to_string_pretty(&config).unwrap();
        assert!(
            !serialized.contains("[themes"),
            "empty themes table should be omitted; got:\n{serialized}"
        );
    }

    #[test]
    fn config_round_trip_musicbrainz_fields() {
        let config = Config {
            musicbrainz_base_url: Some("http://localhost:5000/ws/2".into()),
            musicbrainz_user_agent: Some("zytunes/2.2.0 (you@example.com)".into()),
            ..Config::default()
        };
        let serialized = toml::to_string_pretty(&config).unwrap();
        let deserialized: Config = toml::from_str(&serialized).unwrap();
        assert_eq!(
            deserialized.musicbrainz_base_url.as_deref(),
            Some("http://localhost:5000/ws/2")
        );
        assert_eq!(
            deserialized.musicbrainz_user_agent.as_deref(),
            Some("zytunes/2.2.0 (you@example.com)")
        );
    }

    #[test]
    fn config_backwards_compatible_without_musicbrainz_fields() {
        // Existing configs (predating the MB integration) must continue to parse.
        let toml_str = r#"
theme = "Gruvbox Dark"
music_dir = "/home/user/Music"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.musicbrainz_base_url.is_none());
        assert!(config.musicbrainz_user_agent.is_none());
    }

    #[test]
    fn config_round_trip_transcode_quality() {
        let config = Config {
            transcode_quality: Some("v0".into()),
            ..Config::default()
        };
        let serialized = toml::to_string_pretty(&config).unwrap();
        let deserialized: Config = toml::from_str(&serialized).unwrap();
        assert_eq!(deserialized.transcode_quality.as_deref(), Some("v0"));
    }

    #[test]
    fn config_backwards_compatible_without_transcode_quality() {
        let toml_str = r#"
theme = "Gruvbox Dark"
music_dir = "/home/user/Music"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.transcode_quality.is_none());
    }

    #[test]
    fn config_round_trip_cache_dir() {
        let config = Config {
            cache_dir: Some("/mnt/big/zytunes-cache".into()),
            ..Config::default()
        };
        let serialized = toml::to_string_pretty(&config).unwrap();
        let deserialized: Config = toml::from_str(&serialized).unwrap();
        assert_eq!(
            deserialized.cache_dir.as_deref(),
            Some("/mnt/big/zytunes-cache")
        );
    }

    #[test]
    fn config_round_trip_mtpz_data() {
        let config = Config {
            mtpz_data: Some("/opt/keys/.mtpz-data".into()),
            ..Config::default()
        };
        let serialized = toml::to_string_pretty(&config).unwrap();
        let deserialized: Config = toml::from_str(&serialized).unwrap();
        assert_eq!(
            deserialized.mtpz_data.as_deref(),
            Some("/opt/keys/.mtpz-data")
        );
    }

    #[test]
    fn config_round_trip_album_art_style() {
        for style in ["ascii", "halfblock"] {
            let config = Config {
                album_art_style: Some(style.into()),
                ..Config::default()
            };
            let serialized = toml::to_string_pretty(&config).unwrap();
            let deserialized: Config = toml::from_str(&serialized).unwrap();
            assert_eq!(deserialized.album_art_style.as_deref(), Some(style));
        }
    }

    #[test]
    fn config_round_trip_soundbar_style() {
        for style in ["meters", "eq", "mirror", "pulse", "dots"] {
            let config = Config {
                soundbar_style: Some(style.into()),
                ..Config::default()
            };
            let serialized = toml::to_string_pretty(&config).unwrap();
            let deserialized: Config = toml::from_str(&serialized).unwrap();
            assert_eq!(deserialized.soundbar_style.as_deref(), Some(style));
        }
    }

    #[test]
    fn stems_config_defaults() {
        let cfg = Config::default();
        assert!(cfg.stems.auto_provision());
        assert_eq!(
            cfg.stems
                .resolved_package(zytunes::stems::provision::EngineKind::Demucs),
            "demucs"
        );
        assert_eq!(cfg.stems.model(), "htdemucs_6s");
        assert!(!cfg.stems.gpu());
        assert_eq!(cfg.stems.cache_max_bytes(), 10 * (1 << 30));
        assert!(cfg.stems.command_path().is_none());
    }

    #[test]
    fn stem_cache_dir_honors_override_and_treats_blank_as_unset() {
        // Unset falls back to the default under ~/.cache/zytunes/stems
        // (present whenever $HOME resolves, which it does under test).
        let cfg = StemsConfig::default();
        assert_eq!(
            cfg.stem_cache_dir(),
            zytunes::stems::default_stem_cache_dir()
        );

        // An explicit path wins over the default.
        let cfg = StemsConfig {
            cache_dir: Some("/mnt/big/stems".into()),
            ..StemsConfig::default()
        };
        assert_eq!(
            cfg.stem_cache_dir(),
            Some(std::path::PathBuf::from("/mnt/big/stems"))
        );

        // Blank / whitespace-only behaves like unset, not like a cache at
        // the filesystem root.
        let cfg = StemsConfig {
            cache_dir: Some("   ".into()),
            ..StemsConfig::default()
        };
        assert_eq!(
            cfg.stem_cache_dir(),
            zytunes::stems::default_stem_cache_dir()
        );
    }

    #[test]
    fn stems_config_parses_table() {
        let toml_str = r#"
theme = "BIOS"

[stems]
provision = "manual"
command = "/home/u/.venvs/demucs/bin/demucs"
package = "demucs-next"
model = "htdemucs"
gpu = true
cache_max_gb = 25
"#;
        let cfg: Config = toml::from_str(toml_str).unwrap();
        assert!(!cfg.stems.auto_provision());
        assert_eq!(
            cfg.stems.command_path().as_deref(),
            Some(std::path::Path::new("/home/u/.venvs/demucs/bin/demucs"))
        );
        assert_eq!(
            cfg.stems
                .resolved_package(zytunes::stems::provision::EngineKind::Demucs),
            "demucs-next"
        );
        assert_eq!(cfg.stems.model(), "htdemucs");
        assert!(cfg.stems.gpu());
        assert_eq!(cfg.stems.cache_max_bytes(), 25 * (1 << 30));
    }

    #[test]
    fn stems_recipe_defaults_to_demucs_and_parses_known_values() {
        let cfg = Config::default();
        assert_eq!(
            cfg.stems.recipe_kind(),
            Ok(zytunes::stems::RecipeKind::Demucs)
        );

        for (raw, kind) in [
            ("demucs", zytunes::stems::RecipeKind::Demucs),
            ("hq", zytunes::stems::RecipeKind::Hq),
            ("sw", zytunes::stems::RecipeKind::Sw),
            ("hq-harmony", zytunes::stems::RecipeKind::HqHarmony),
        ] {
            let cfg: Config = toml::from_str(&format!("[stems]\nrecipe = \"{raw}\"\n")).unwrap();
            assert_eq!(cfg.stems.recipe_kind(), Ok(kind));
        }

        // Blank behaves like unset; unknown surfaces the raw value so
        // the caller can log it before falling back.
        let cfg: Config = toml::from_str("[stems]\nrecipe = \"\"\n").unwrap();
        assert_eq!(
            cfg.stems.recipe_kind(),
            Ok(zytunes::stems::RecipeKind::Demucs)
        );
        let cfg: Config = toml::from_str("[stems]\nrecipe = \"roformer\"\n").unwrap();
        assert_eq!(cfg.stems.recipe_kind(), Err("roformer".to_string()));
    }

    #[test]
    fn stems_resolved_package_prefers_explicit_then_engine_default() {
        use zytunes::stems::provision::EngineKind;
        let cfg: Config = toml::from_str("[stems]\npackage = \"demucs-next\"\n").unwrap();
        // An explicit package wins only for the engine whose family it
        // names — a demucs-era override leaking into an audio-separator
        // install would "succeed" and then dead-end on the missing
        // engine binary.
        assert_eq!(
            cfg.stems.resolved_package(EngineKind::Demucs),
            "demucs-next"
        );
        assert_eq!(
            cfg.stems.resolved_package(EngineKind::AudioSeparator),
            EngineKind::AudioSeparator.default_package(false)
        );
        // And the reverse: an audio-separator pin stays off demucs.
        let cfg: Config =
            toml::from_str("[stems]\npackage = \"audio-separator[gpu]==0.44.3\"\n").unwrap();
        assert_eq!(
            cfg.stems.resolved_package(EngineKind::AudioSeparator),
            "audio-separator[gpu]==0.44.3"
        );
        assert_eq!(cfg.stems.resolved_package(EngineKind::Demucs), "demucs");

        // Unset: each engine's default, honoring the gpu flag.
        let cfg = Config::default();
        assert_eq!(cfg.stems.resolved_package(EngineKind::Demucs), "demucs");
        assert_eq!(
            cfg.stems.resolved_package(EngineKind::AudioSeparator),
            EngineKind::AudioSeparator.default_package(false)
        );
        let cfg: Config = toml::from_str("[stems]\ngpu = true\n").unwrap();
        assert_eq!(
            cfg.stems.resolved_package(EngineKind::AudioSeparator),
            EngineKind::AudioSeparator.default_package(true)
        );
    }

    #[test]
    fn stems_config_empty_command_reads_as_unset() {
        let cfg: Config = toml::from_str("[stems]\ncommand = \"\"\n").unwrap();
        assert!(cfg.stems.command_path().is_none());
    }

    #[test]
    fn empty_stems_table_is_not_serialized() {
        let serialized = toml::to_string_pretty(&Config::default()).unwrap();
        assert!(
            !serialized.contains("[stems]"),
            "default stems table should be omitted; got:\n{serialized}"
        );
    }

    #[test]
    fn stems_config_round_trips() {
        let config = Config {
            stems: StemsConfig {
                command: Some("/x/demucs".into()),
                cache_max_gb: Some(5),
                ..StemsConfig::default()
            },
            ..Config::default()
        };
        let serialized = toml::to_string_pretty(&config).unwrap();
        let deserialized: Config = toml::from_str(&serialized).unwrap();
        assert_eq!(deserialized.stems, config.stems);
    }

    #[test]
    fn update_contents_writes_new_file_when_missing() {
        let out = update_contents(None, |c| c.theme = Some("BIOS".into()))
            .expect("should produce output");
        assert!(out.contains("theme = \"BIOS\""));
    }

    #[test]
    fn update_contents_preserves_other_fields() {
        let existing = r#"theme = "Gruvbox Dark"
music_dir = "/home/user/Music"
"#;
        let out = update_contents(Some(existing), |c| c.album_art_style = Some("ascii".into()))
            .expect("should produce output");
        assert!(out.contains("theme = \"Gruvbox Dark\""));
        assert!(out.contains("music_dir = \"/home/user/Music\""));
        assert!(out.contains("album_art_style = \"ascii\""));
    }

    #[test]
    fn update_contents_refuses_to_clobber_unparseable_file() {
        let existing = "this is not valid toml ===";
        let out = update_contents(Some(existing), |c| c.album_art_style = Some("ascii".into()));
        assert!(
            out.is_none(),
            "update must not produce output when the existing file is corrupt"
        );
    }
}
