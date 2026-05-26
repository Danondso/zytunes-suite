use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Default)]
pub struct Config {
    pub theme: Option<String>,
    pub music_dir: Option<String>,
    pub photo_dir: Option<String>,
    pub video_dir: Option<String>,
    pub album_art_style: Option<String>,
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
    /// Default auto-eject preference for the CD import overlay. The
    /// overlay's own checkbox can override per-import. Unset defaults
    /// to `true` so a successful rip ejects the disc.
    pub cd_auto_eject: Option<bool>,
    /// User-defined custom themes keyed by theme name. Each entry inherits
    /// missing fields from its `base` (or `iTunes 2004` when unset) and merges
    /// into the theme picker alongside the built-ins.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub themes: BTreeMap<String, UserTheme>,
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
        assert!(config.album_art_style.is_none());
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
        assert!(config.album_art_style.is_none());
        assert!(config.show_player.is_none());
        assert!(config.fingerprinting.is_none());
        assert!(config.acoustid_fingerprint.is_none());
        assert!(config.musicbrainz_base_url.is_none());
        assert!(config.musicbrainz_user_agent.is_none());
        assert!(config.default_fidelity.is_none());
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
