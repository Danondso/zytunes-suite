//! Philips GoGear session: plain filesystem copy under `MUSIC/`.
//!
//! The ViBE indexes ID3 tags after disconnect; we do not write a media
//! database. `_system/` is firmware-owned and is never listed, imported
//! into, or deleted.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use lofty::file::{AudioFile, TaggedFileExt};
use lofty::prelude::Accessor;

use super::parse::DeviceEntry;
use super::{DeviceError, DeviceSession, TrackMeta};

const AUDIO_EXTS: &[&str] = &["mp3", "wma", "wav"];

pub struct GogearSession {
    mount: PathBuf,
    /// Display path (`Artist/Album/Title.ext`) → on-disk file, rebuilt on
    /// collect/import so `rm` can resolve TUI rows.
    by_display: HashMap<String, PathBuf>,
}

impl GogearSession {
    pub fn new(mount: PathBuf) -> Self {
        Self {
            mount,
            by_display: HashMap::new(),
        }
    }

    fn music_dir(&self) -> PathBuf {
        // ViBE ships `Music/`; vfat is case-insensitive but ext/test
        // dirs are not, so prefer an existing Music/MUSIC folder.
        if let Ok(rd) = std::fs::read_dir(&self.mount) {
            for e in rd.flatten() {
                if e.file_name().eq_ignore_ascii_case("Music") && e.path().is_dir() {
                    return e.path();
                }
            }
        }
        self.mount.join("MUSIC")
    }

    fn resolve(&self, path: &str) -> PathBuf {
        let stripped = path.trim_start_matches('/').replace('\\', "/");
        if stripped.is_empty() {
            self.mount.clone()
        } else if stripped.eq_ignore_ascii_case("MUSIC") {
            self.music_dir()
        } else if let Some(rest) = stripped
            .strip_prefix("MUSIC/")
            .or_else(|| stripped.strip_prefix("Music/"))
        {
            self.music_dir().join(rest)
        } else {
            self.mount.join(stripped)
        }
    }
}

fn is_skipped_name(name: &str) -> bool {
    matches!(
        name,
        "_system"
            | "System Volume Information"
            | ".Trash"
            | ".Trashes"
            | ".fseventsd"
            | ".Spotlight-V100"
            | "FOUND.000"
    )
}

fn is_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| AUDIO_EXTS.iter().any(|a| e.eq_ignore_ascii_case(a)))
}

fn sanitise(s: &str) -> String {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return "Unknown".into();
    }
    let mut out = String::with_capacity(trimmed.len());
    for c in trimmed.chars() {
        match c {
            '/' | '\\' | '\0' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => out.push('_'),
            c if (c as u32) < 0x20 => {}
            c => out.push(c),
        }
    }
    while out.ends_with('.') || out.ends_with(' ') {
        out.pop();
    }
    if out.is_empty() {
        "Unknown".into()
    } else {
        out
    }
}

fn unique_dest(dir: &Path, file_name: &str) -> PathBuf {
    let dest = dir.join(file_name);
    if !dest.exists() {
        return dest;
    }
    let stem = Path::new(file_name)
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy();
    let ext = Path::new(file_name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("mp3");
    for n in 2..1000 {
        let candidate = dir.join(format!("{stem}-{n}.{ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    dir.join(format!("{stem}-dup.{ext}"))
}

fn tags_for(path: &Path) -> (String, String, String, Option<u32>, Option<u32>) {
    let fallback = path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let tagged = lofty::read_from_path(path).ok();
    let tag = tagged.as_ref().and_then(|f| f.primary_tag());
    let title = tag
        .and_then(|t| t.title().map(|s| s.to_string()))
        .filter(|s| !s.is_empty())
        .unwrap_or(fallback);
    let artist = tag
        .and_then(|t| t.artist().map(|s| s.to_string()))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Unknown Artist".into());
    let album = tag
        .and_then(|t| t.album().map(|s| s.to_string()))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Unknown Album".into());
    let track_number = tag.and_then(|t| t.track());
    let duration_ms = tagged
        .as_ref()
        .map(|f| f.properties().duration().as_millis() as u32)
        .filter(|&ms| ms > 0);
    (artist, album, title, track_number, duration_ms)
}

impl DeviceSession for GogearSession {
    fn ls(&mut self, path: &str) -> Result<Vec<DeviceEntry>, DeviceError> {
        let real = self.resolve(path);
        if !real.exists() {
            return Ok(Vec::new());
        }
        let mut result = Vec::new();
        let entries = std::fs::read_dir(&real)
            .map_err(|e| format!("Cannot list {}: {}", real.display(), e))?;
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if is_skipped_name(&name) {
                continue;
            }
            let meta = entry.metadata().ok();
            let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
            let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
            result.push(DeviceEntry {
                object_id: 0,
                storage_id: 0,
                format: if is_dir {
                    "Association".into()
                } else {
                    name.rsplit('.').next().unwrap_or("unknown").to_uppercase()
                },
                size,
                name,
                ..Default::default()
            });
        }
        Ok(result)
    }

    fn import_track(
        &mut self,
        local_path: &str,
        meta: Option<&TrackMeta>,
    ) -> Result<u64, DeviceError> {
        let src = PathBuf::from(local_path);
        if !src.exists() {
            return Err(format!("File not found: {local_path}").into());
        }
        let (artist, album, _title, _, _) = if let Some(m) = meta {
            (
                m.artist.clone(),
                m.album.clone(),
                m.title.clone(),
                m.track_number,
                None,
            )
        } else {
            tags_for(&src)
        };
        let ext = src
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("mp3")
            .to_lowercase();
        let orig = src
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let file_name = format!("{}.{ext}", sanitise(&orig));
        let dest_dir = self
            .music_dir()
            .join(sanitise(&artist))
            .join(sanitise(&album));
        std::fs::create_dir_all(&dest_dir)
            .map_err(|e| format!("Cannot create {}: {e}", dest_dir.display()))?;
        let dest = unique_dest(&dest_dir, &file_name);
        std::fs::copy(&src, &dest).map_err(|e| format!("Copy to {}: {e}", dest.display()))?;
        let display = format!(
            "{}/{}/{}",
            sanitise(&artist),
            sanitise(&album),
            dest.file_name().unwrap_or_default().to_string_lossy()
        );
        self.by_display.insert(display, dest.clone());
        let ino = std::fs::metadata(&dest)
            .ok()
            .map(|m| {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    m.ino()
                }
                #[cfg(not(unix))]
                {
                    let _ = m;
                    0u64
                }
            })
            .unwrap_or(0);
        Ok(ino)
    }

    fn rm(&mut self, device_path: &str) -> Result<(), DeviceError> {
        let key = device_path.trim_start_matches('/');
        let real = self
            .by_display
            .get(key)
            .cloned()
            .unwrap_or_else(|| self.resolve(device_path));
        if real.is_dir() {
            std::fs::remove_dir_all(&real)
                .map_err(|e| format!("Cannot remove {}: {e}", real.display()))?;
        } else if real.is_file() {
            std::fs::remove_file(&real)
                .map_err(|e| format!("Cannot remove {}: {e}", real.display()))?;
        } else {
            return Err(format!("Not found: {device_path}").into());
        }
        self.by_display.retain(|_, p| p != &real);
        Ok(())
    }

    fn rm_by_id(&mut self, object_id: u32) -> Result<(), DeviceError> {
        let found = self.by_display.iter().find_map(|(_, p)| {
            let meta = std::fs::metadata(p).ok()?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                (meta.ino() as u32 == object_id).then(|| p.clone())
            }
            #[cfg(not(unix))]
            {
                let _ = meta;
                None
            }
        });
        let Some(path) = found else {
            return Err(format!("No GoGear file with id {object_id:#x}").into());
        };
        let display = self
            .by_display
            .iter()
            .find(|(_, p)| *p == &path)
            .map(|(k, _)| k.clone())
            .unwrap_or_default();
        self.rm(&display)
    }

    fn cleanup_empty_folders(&mut self) -> Result<usize, DeviceError> {
        let music = self.music_dir();
        if !music.is_dir() {
            return Ok(0);
        }
        let mut removed = 0;
        let artists: Vec<PathBuf> = std::fs::read_dir(&music)
            .map_err(|e| format!("Cannot list {}: {e}", music.display()))?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        for artist in artists {
            let albums: Vec<PathBuf> = std::fs::read_dir(&artist)
                .map(|rd| {
                    rd.flatten()
                        .map(|e| e.path())
                        .filter(|p| p.is_dir())
                        .collect()
                })
                .unwrap_or_default();
            for album in albums {
                if std::fs::read_dir(&album)
                    .map(|mut d| d.next().is_none())
                    .unwrap_or(false)
                {
                    let _ = std::fs::remove_dir(&album);
                    removed += 1;
                }
            }
            if std::fs::read_dir(&artist)
                .map(|mut d| d.next().is_none())
                .unwrap_or(false)
            {
                let _ = std::fs::remove_dir(&artist);
                removed += 1;
            }
        }
        Ok(removed)
    }

    fn get_storage_info(&mut self) -> Result<(u64, u64), DeviceError> {
        let output = std::process::Command::new("df")
            .args(["-k", &self.mount.to_string_lossy()])
            .output()
            .map_err(|e| format!("Failed to run df: {e}"))?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let line = stdout.lines().nth(1).ok_or("df returned no data")?;
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 4 {
            return Err("Unexpected df output format".into());
        }
        let total_kb: u64 = cols[1].parse().unwrap_or(0);
        let available_kb: u64 = cols[3].parse().unwrap_or(0);
        Ok((total_kb * 1024, available_kb * 1024))
    }

    fn collect_all_tracks(&mut self, _path: &str) -> Result<Vec<DeviceEntry>, DeviceError> {
        self.by_display.clear();
        let mut files = Vec::new();
        let music = self.music_dir();
        if music.is_dir() {
            walk_audio(&music, &mut files);
        } else {
            walk_audio(&self.mount, &mut files);
        }
        let mut entries = Vec::new();
        for path in files {
            let (artist, album, title, track_number, duration_ms) = tags_for(&path);
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("mp3")
                .to_lowercase();
            let display = format!(
                "{}/{}/{}.{}",
                sanitise(&artist).replace('/', "_"),
                sanitise(&album).replace('/', "_"),
                sanitise(&title).replace('/', "_"),
                ext
            );
            let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            let object_id = {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    std::fs::metadata(&path).map(|m| m.ino()).unwrap_or(0)
                }
                #[cfg(not(unix))]
                {
                    0u64
                }
            };
            self.by_display.insert(display.clone(), path);
            entries.push(DeviceEntry {
                object_id,
                storage_id: 0,
                format: ext.to_uppercase(),
                size,
                name: display,
                track_number,
                duration_ms,
                ..Default::default()
            });
        }
        Ok(entries)
    }
}

fn walk_audio(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for ent in rd.flatten() {
        let path = ent.path();
        let name = ent.file_name().to_string_lossy().into_owned();
        if is_skipped_name(&name) {
            continue;
        }
        if path.is_dir() {
            walk_audio(&path, out);
        } else if is_audio(&path) {
            out.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> (PathBuf, GogearSession) {
        let dir = std::env::temp_dir().join(format!(
            "zytunes-gogear-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let session = GogearSession::new(dir.clone());
        (dir, session)
    }

    fn write_mp3(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"not-a-real-mp3").unwrap();
    }

    #[test]
    fn import_lands_under_music_artist_album() {
        let (dir, mut s) = tmp();
        let src = dir.join("src.mp3");
        write_mp3(&src);
        let meta = TrackMeta {
            artist: "A/rtist".into(),
            album: "Album".into(),
            title: "Song".into(),
            track_number: Some(1),
            genre: None,
        };
        s.import_track(src.to_str().unwrap(), Some(&meta)).unwrap();
        let dest = s.music_dir().join("A_rtist").join("Album").join("src.mp3");
        assert!(dest.is_file(), "missing {}", dest.display());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn collect_skips_system_folder() {
        let (dir, mut s) = tmp();
        write_mp3(&dir.join("_system").join("hidden.mp3"));
        write_mp3(&dir.join("MUSIC").join("Art").join("Alb").join("t.mp3"));
        let tracks = s.collect_all_tracks("/MUSIC").unwrap();
        assert_eq!(tracks.len(), 1);
        assert!(tracks[0].name.ends_with(".mp3"));
        assert!(!tracks.iter().any(|t| t.name.contains("_system")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rm_deletes_imported_file() {
        let (dir, mut s) = tmp();
        let src = dir.join("gone.mp3");
        write_mp3(&src);
        s.import_track(
            src.to_str().unwrap(),
            Some(&TrackMeta {
                artist: "Art".into(),
                album: "Alb".into(),
                title: "Gone".into(),
                track_number: None,
                genre: None,
            }),
        )
        .unwrap();
        let tracks = s.collect_all_tracks("/MUSIC").unwrap();
        assert_eq!(tracks.len(), 1);
        s.rm(&tracks[0].name).unwrap();
        assert!(s.collect_all_tracks("/MUSIC").unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ls_hides_system_dir() {
        let (dir, mut s) = tmp();
        std::fs::create_dir(dir.join("_system")).unwrap();
        std::fs::create_dir(dir.join("MUSIC")).unwrap();
        let names: Vec<_> = s.ls("/").unwrap().into_iter().map(|e| e.name).collect();
        assert!(names.contains(&"MUSIC".into()));
        assert!(!names.contains(&"_system".into()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
