use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Default)]
pub struct Config {
    pub theme: Option<String>,
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
