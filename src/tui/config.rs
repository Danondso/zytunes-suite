use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Default)]
pub struct Config {
    pub theme: Option<String>,
    pub music_dir: Option<String>,
    pub photo_dir: Option<String>,
    pub video_dir: Option<String>,
    pub album_art_style: Option<String>,
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
    }

    #[test]
    fn config_round_trip_with_new_fields() {
        let config = Config {
            theme: Some("BIOS".into()),
            music_dir: Some("/music".into()),
            photo_dir: Some("/photos".into()),
            video_dir: Some("/videos".into()),
            album_art_style: Some("ascii".into()),
        };
        let serialized = toml::to_string_pretty(&config).unwrap();
        let deserialized: Config = toml::from_str(&serialized).unwrap();
        assert_eq!(deserialized.photo_dir.as_deref(), Some("/photos"));
        assert_eq!(deserialized.video_dir.as_deref(), Some("/videos"));
        assert_eq!(deserialized.album_art_style.as_deref(), Some("ascii"));
    }

    #[test]
    fn config_default_all_none() {
        let config = Config::default();
        assert!(config.theme.is_none());
        assert!(config.music_dir.is_none());
        assert!(config.photo_dir.is_none());
        assert!(config.video_dir.is_none());
        assert!(config.album_art_style.is_none());
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
