use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Default)]
pub struct Config {
    pub theme: Option<String>,
    pub music_dir: Option<String>,
    pub photo_dir: Option<String>,
    pub video_dir: Option<String>,
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

pub fn save(config: &Config) {
    let dir = match config_dir() {
        Some(d) => d,
        None => return,
    };
    let path = dir.join("config.toml");
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(contents) = toml::to_string_pretty(config) {
        let _ = std::fs::write(path, contents);
    }
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
    }

    #[test]
    fn config_round_trip_with_new_fields() {
        let config = Config {
            theme: Some("BIOS".into()),
            music_dir: Some("/music".into()),
            photo_dir: Some("/photos".into()),
            video_dir: Some("/videos".into()),
        };
        let serialized = toml::to_string_pretty(&config).unwrap();
        let deserialized: Config = toml::from_str(&serialized).unwrap();
        assert_eq!(deserialized.photo_dir.as_deref(), Some("/photos"));
        assert_eq!(deserialized.video_dir.as_deref(), Some("/videos"));
    }

    #[test]
    fn config_default_all_none() {
        let config = Config::default();
        assert!(config.theme.is_none());
        assert!(config.music_dir.is_none());
        assert!(config.photo_dir.is_none());
        assert!(config.video_dir.is_none());
    }
}
