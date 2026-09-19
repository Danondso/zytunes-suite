// Self-alias the crate so TUI source files re-included via `#[path]` (gated
// by `tui-testing`) can keep referring to lib items as `zytunes::*` exactly
// like they do when compiled as part of the `zytunes-tui` binary, where the
// lib is a normal external dependency.
#[cfg(feature = "tui-testing")]
extern crate self as zytunes;

pub mod acoustid;
pub mod art_cache;
pub mod cache;
pub mod cd;
pub mod device;
pub mod dirlib;
pub mod fingerprint;
pub mod genre_norm;
pub mod library;
pub mod listen_log;
pub mod local_plays;
pub mod mtp;
pub mod musicbrainz;
pub mod paths;
pub mod playlist;
pub mod playlist_store;
pub mod recommender;
pub mod stems;
pub mod tag_ops;
pub mod transcode;

#[cfg(test)]
mod test_audio;

// TUI modules re-exposed through the lib crate so integration tests can drive
// the App against a ratatui TestBackend. Declared at the crate root (not under
// a `tui::` namespace) so the same `crate::audio` / `crate::background` /
// etc. paths inside the module sources resolve identically whether they're
// compiled as part of the `zytunes-tui` binary or as part of this lib.
#[cfg(feature = "tui-testing")]
#[path = "tui/anim.rs"]
pub mod anim;
#[cfg(feature = "tui-testing")]
#[path = "tui/app.rs"]
pub mod app;
#[cfg(feature = "tui-testing")]
#[path = "tui/audio.rs"]
pub mod audio;
#[cfg(feature = "tui-testing")]
#[path = "tui/background.rs"]
pub mod background;
#[cfg(feature = "tui-testing")]
#[path = "tui/config.rs"]
pub mod config;
#[cfg(feature = "tui-testing")]
#[path = "tui/testing.rs"]
pub mod testing;
#[cfg(feature = "tui-testing")]
#[path = "tui/theme.rs"]
pub mod theme;
#[cfg(feature = "tui-testing")]
#[path = "tui/ui/mod.rs"]
pub mod ui;

use device::{
    DetectedDevice, DeviceBackend, DeviceCapabilities, DeviceFamily, IpodBackend, ZuneBackend,
};
use library::MusicLibrary;
use mtp::DeviceSession;
use std::fmt;
use std::path::Path;
use std::str::FromStr;

// Re-exported so existing callers (CLI, TUI worker) keep importing the
// transcode pipeline from the crate root.
pub use transcode::{
    check_ffmpeg_available, make_transcode_temp_dir, needs_transcoding, needs_video_transcoding,
    transcode_and_import, transcode_and_import_video, transcode_flac_to_alac, transcode_for_device,
    transcode_to_mp3, transcode_to_wmv,
};

/// The type of sync operation to perform.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SyncType {
    Artist,
    Album,
    Track,
}

impl FromStr for SyncType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "artist" => Ok(SyncType::Artist),
            "album" => Ok(SyncType::Album),
            "track" => Ok(SyncType::Track),
            other => Err(format!(
                "Unknown sync type: \"{other}\". Use: artist, album, track"
            )),
        }
    }
}

impl fmt::Display for SyncType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SyncType::Artist => write!(f, "artist"),
            SyncType::Album => write!(f, "album"),
            SyncType::Track => write!(f, "track"),
        }
    }
}

/// Resolve the music directory from either `ZYTUNES_MUSIC_DIR` or `music_dir`.
///
/// Returns the first path that exists and is a directory. Used by both the CLI
/// (via `load_library`) and the TUI (which calls `DirectoryLibrary::scan_with_progress`
/// directly so it can stream progress events).
pub fn resolve_music_dir(music_dir: Option<&str>) -> Result<String, String> {
    if let Ok(dir) = std::env::var("ZYTUNES_MUSIC_DIR") {
        if Path::new(&dir).is_dir() {
            return Ok(dir);
        }
    }

    if let Some(dir) = music_dir {
        if Path::new(dir).is_dir() {
            return Ok(dir.to_string());
        }
    }

    Err(
        "No music library found. Set ZYTUNES_MUSIC_DIR to a music folder, \
         or add music_dir to ~/.config/zytunes/config.toml"
            .into(),
    )
}

/// Load a music library by scanning a directory.
///
/// Tries `ZYTUNES_MUSIC_DIR`, then the `music_dir` argument (typically from
/// config.toml). Returns a boxed trait object so callers are backend-agnostic.
/// Always uses default scan options (fingerprinting enabled). Callers that
/// want to disable fingerprinting should construct `ScanOptions` and call
/// `dirlib::DirectoryLibrary::scan_with_options` directly.
pub fn load_library(music_dir: Option<&str>) -> Result<Box<dyn MusicLibrary + Send>, String> {
    load_library_with_options(music_dir, dirlib::ScanOptions::default())
}

/// Like `load_library`, but lets the caller override scan options.
pub fn load_library_with_options(
    music_dir: Option<&str>,
    options: dirlib::ScanOptions,
) -> Result<Box<dyn MusicLibrary + Send>, String> {
    let dir = resolve_music_dir(music_dir)?;
    dirlib::DirectoryLibrary::scan_with_options(&dir, options, |_| {})
        .map(|l| Box::new(l) as Box<dyn MusicLibrary + Send>)
}

/// Formats the Zune natively supports (no transcoding needed).
pub const ZUNE_NATIVE_FORMATS: &[&str] = &["mp3", "wma", "aac"];

/// Connect to a supported device using the backend registry.
///
/// Tries each registered backend in order, returning the first successful
/// session along with the device's capabilities and detection info.
pub fn connect() -> Result<
    (
        Box<dyn DeviceSession + Send>,
        DeviceCapabilities,
        DetectedDevice,
    ),
    String,
> {
    let backends: Vec<Box<dyn DeviceBackend>> = vec![Box::new(ZuneBackend), Box::new(IpodBackend)];

    let mut last_err = String::from("No devices detected");
    for backend in &backends {
        let Ok(detected) = backend.detect() else {
            continue;
        };
        println!(
            "{} detected: {}",
            detected.name,
            match detected.family {
                DeviceFamily::Zune => "Zune",
                DeviceFamily::Ipod => "iPod",
            }
        );

        print!("Connecting... ");
        match backend.open_session(&detected, None) {
            Ok(session) => {
                println!("OK");
                let caps = backend.capabilities();
                return Ok((session, caps, detected));
            }
            Err(e) => {
                println!("FAILED");
                last_err = e;
            }
        }
    }

    Err(last_err)
}

/// Strip leading track number from a filename stem.
/// "310 Oh Johnny, Oh Johnny, Oh" -> "Oh Johnny, Oh Johnny, Oh"
/// "02 To You" -> "To You"
/// "Blue Rain" -> "Blue Rain" (no number prefix)
pub fn strip_track_number(s: &str) -> &str {
    match s.split_once(' ') {
        Some((prefix, rest)) if prefix.chars().all(|c| c.is_ascii_digit()) => rest.trim(),
        _ => s,
    }
}

/// Find matching tracks from the library for the given sync type and name.
pub fn find_matching_tracks<'a>(
    lib: &'a dyn MusicLibrary,
    sync_type: SyncType,
    name: &str,
) -> Result<Vec<&'a library::Track>, String> {
    match sync_type {
        SyncType::Artist => {
            let tracks: Vec<&library::Track> = lib.artist_tracks(name).collect();
            if tracks.is_empty() {
                let mut msg = format!("No tracks found for artist \"{}\"", name);
                let artists = lib.artists();
                let suggestions: Vec<&&str> = artists
                    .iter()
                    .filter(|a| a.to_lowercase().contains(&name.to_lowercase()))
                    .take(5)
                    .collect();
                if !suggestions.is_empty() {
                    msg.push_str("\nDid you mean:");
                    for s in suggestions {
                        msg.push_str(&format!("\n  {s}"));
                    }
                }
                return Err(msg);
            }
            println!("Artist \"{}\": {} tracks", name, tracks.len());
            Ok(tracks)
        }
        SyncType::Album => {
            let tracks: Vec<&library::Track> = lib.album_tracks(name).collect();
            if tracks.is_empty() {
                return Err(format!("No tracks found for album \"{}\"", name));
            }
            println!("Album \"{}\": {} tracks", name, tracks.len());
            Ok(tracks)
        }
        SyncType::Track => {
            let tracks: Vec<&library::Track> = lib.tracks_by_name(name).collect();
            if tracks.is_empty() {
                return Err(format!("No track found matching \"{}\"", name));
            }
            println!("Track \"{}\": {} match(es)", name, tracks.len());
            Ok(tracks)
        }
    }
}

/// Result of a sync operation.
pub struct SyncResult {
    pub success: usize,
    pub failed: usize,
    pub skipped: usize,
}

/// Sync tracks to the device: scan for duplicates and import new tracks.
pub fn sync_to_device(
    session: &mut dyn DeviceSession,
    pushable: &[&library::Track],
    temp_dir: &Path,
    caps: &DeviceCapabilities,
) -> Result<SyncResult, String> {
    // Scan device for existing tracks to avoid duplicates.
    print!("Scanning device for existing tracks... ");
    let existing_tracks = session
        .collect_all_tracks(caps.music_root)
        .unwrap_or_default();
    let existing_names: std::collections::HashSet<String> = existing_tracks
        .iter()
        .map(|t| {
            // Device path format: "Artist/Album/track.ext" — extract artist and track stem.
            let parts: Vec<&str> = t.name.splitn(3, '/').collect();
            let artist = parts.first().unwrap_or(&"").to_lowercase();
            let fallback = t.name.as_str();
            let filename = parts.last().unwrap_or(&fallback);
            let stem = filename.rsplit('.').next_back().unwrap_or(filename);
            format!("{}/{}", artist, strip_track_number(stem).to_lowercase())
        })
        .collect();
    println!("{} tracks on device", existing_tracks.len());

    // Filter out tracks already on the device.
    let mut to_push: Vec<&library::Track> = Vec::new();
    let mut skipped = 0;
    for track in pushable {
        let key = format!(
            "{}/{}",
            track.artist.to_lowercase(),
            track.name.to_lowercase()
        );
        if existing_names.contains(&key) {
            skipped += 1;
        } else {
            to_push.push(track);
        }
    }

    if skipped > 0 {
        println!("{} tracks already on device (skipped)", skipped);
    }

    let mut success = 0;
    let mut failed = 0;
    let total = to_push.len();

    if total == 0 {
        println!("\nAll tracks already on device — nothing to sync");
    }

    for (i, track) in to_push.iter().enumerate() {
        let loc = match track.location.as_deref() {
            Some(l) => l,
            None => {
                println!("  SKIPPED (no file location): {}", track.name);
                failed += 1;
                continue;
            }
        };
        let display = format!("{} - {} - {}", track.artist, track.album, track.name);
        println!("[{}/{}] {}", i + 1, total, display);

        let meta = mtp::TrackMeta::from_track(track);
        match transcode_and_import(session, loc, temp_dir, caps, Some(&meta)) {
            Ok(object_id) => {
                println!("  OK (id: {})", object_id);
                success += 1;
            }
            Err(e) => {
                println!("  FAILED: {}", e);
                failed += 1;
            }
        }
    }

    Ok(SyncResult {
        success,
        failed,
        skipped,
    })
}

/// Expand paths into a list of music files.
/// If a path is a directory, recursively find music files in it.
///
/// Diagnostic messages (skipped files, missing paths) go to stderr.
pub fn collect_music_files(paths: &[&str]) -> Vec<String> {
    collect_music_files_with_logger(paths, &cache::default_logger())
}

/// Like [`collect_music_files`] but routes diagnostics through `log`.
pub fn collect_music_files_with_logger(paths: &[&str], log: &cache::Logger) -> Vec<String> {
    let music_extensions = [
        "mp3", "wma", "aac", "m4a", "ogg", "flac", "wav", "opus", "alac", "aiff",
    ];
    collect_files_by_extension(paths, &music_extensions, "music", log)
}

pub fn collect_music_files_recursive(dir: &Path, extensions: &[&str], files: &mut Vec<String>) {
    collect_files_recursive_with_logger(dir, extensions, files, &cache::default_logger())
}

/// Expand paths into a list of photo files.
/// If a path is a directory, recursively find photo files in it.
///
/// Diagnostic messages (skipped files, missing paths) go to stderr.
pub fn collect_photo_files(paths: &[&str]) -> Vec<String> {
    collect_photo_files_with_logger(paths, &cache::default_logger())
}

/// Like [`collect_photo_files`] but routes diagnostics through `log`.
pub fn collect_photo_files_with_logger(paths: &[&str], log: &cache::Logger) -> Vec<String> {
    let photo_extensions = ["jpg", "jpeg", "png", "bmp", "gif", "tiff", "webp"];
    collect_files_by_extension(paths, &photo_extensions, "photo", log)
}

/// Expand paths into a list of video files.
/// If a path is a directory, recursively find video files in it.
///
/// Diagnostic messages (skipped files, missing paths) go to stderr.
pub fn collect_video_files(paths: &[&str]) -> Vec<String> {
    collect_video_files_with_logger(paths, &cache::default_logger())
}

/// Like [`collect_video_files`] but routes diagnostics through `log`.
pub fn collect_video_files_with_logger(paths: &[&str], log: &cache::Logger) -> Vec<String> {
    let video_extensions = ["wmv", "mp4", "avi", "mpeg", "mpg"];
    collect_files_by_extension(paths, &video_extensions, "video", log)
}

/// Shared body of the `collect_*_files_with_logger` helpers: expand each
/// path (file or directory) into the sorted list of files matching
/// `extensions`. `kind` names the media type in skip diagnostics
/// ("Skipping non-music file: …").
fn collect_files_by_extension(
    paths: &[&str],
    extensions: &[&str],
    kind: &str,
    log: &cache::Logger,
) -> Vec<String> {
    let mut files = Vec::new();

    for path in paths {
        let p = Path::new(path);
        if p.is_file() {
            if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
                if extensions.contains(&ext.to_lowercase().as_str()) {
                    files.push(path.to_string());
                } else {
                    log(&format!("Skipping non-{kind} file: {path}"));
                }
            }
        } else if p.is_dir() {
            collect_files_recursive_with_logger(p, extensions, &mut files, log);
        } else {
            log(&format!("Not found: {path}"));
        }
    }

    files.sort();
    files
}

/// Generic recursive file collection by extension list.
fn collect_files_recursive_with_logger(
    dir: &Path,
    extensions: &[&str],
    files: &mut Vec<String>,
    log: &cache::Logger,
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            log(&format!("Cannot read directory {}: {}", dir.display(), e));
            return;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files_recursive_with_logger(&path, extensions, files, log);
        } else if path.is_file() {
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if extensions.contains(&ext.to_lowercase().as_str()) {
                    files.push(path.to_string_lossy().to_string());
                }
            }
        }
    }
}

/// Resize a photo to fit within the Zune screen (240x320) and encode as JPEG.
/// Preserves aspect ratio using Lanczos3 downsampling.
pub fn resize_photo_for_zune(path: &str) -> Result<Vec<u8>, String> {
    let img = image::open(path).map_err(|e| format!("Cannot open image {}: {}", path, e))?;
    let resized = img.resize(240, 320, image::imageops::FilterType::Lanczos3);
    let mut jpeg_buf = std::io::Cursor::new(Vec::new());
    resized
        .write_to(&mut jpeg_buf, image::ImageFormat::Jpeg)
        .map_err(|e| format!("Failed to encode JPEG: {}", e))?;
    let data = jpeg_buf.into_inner();
    if data.is_empty() {
        return Err("Encoded JPEG is empty".into());
    }
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ZUNE_FORMATS: &[&str] = &["mp3", "wma", "aac"];

    fn test_caps() -> DeviceCapabilities {
        DeviceCapabilities {
            family: DeviceFamily::Zune,
            supported_formats: ZUNE_FORMATS,
            transcode_target: "mp3",
            lossless_target: None,
            music_root: "/Music",
            max_art_dimensions: Some((200, 200)),
        }
    }

    /// Minimal in-memory `MusicLibrary` for unit tests.
    struct TestLibrary {
        tracks: Vec<library::Track>,
    }

    impl MusicLibrary for TestLibrary {
        fn artists(&self) -> Vec<&str> {
            let mut v: Vec<&str> = self
                .tracks
                .iter()
                .map(|t| t.artist.as_str())
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .collect();
            v.sort_unstable();
            v
        }
        fn albums(&self) -> Vec<(&str, &str)> {
            let mut v: Vec<(&str, &str)> = self
                .tracks
                .iter()
                .map(|t| (t.artist.as_str(), t.album.as_str()))
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .collect();
            v.sort_unstable();
            v
        }
        fn artist_tracks<'a>(
            &'a self,
            artist: &str,
        ) -> Box<dyn Iterator<Item = &'a library::Track> + 'a> {
            let artist = artist.to_string();
            Box::new(
                self.tracks
                    .iter()
                    .filter(move |t| t.artist.eq_ignore_ascii_case(&artist)),
            )
        }
        fn album_tracks<'a>(
            &'a self,
            album: &str,
        ) -> Box<dyn Iterator<Item = &'a library::Track> + 'a> {
            let album = album.to_string();
            Box::new(
                self.tracks
                    .iter()
                    .filter(move |t| t.album.eq_ignore_ascii_case(&album)),
            )
        }
        fn album_tracks_by_artist<'a>(
            &'a self,
            artist: &str,
            album: &str,
        ) -> Box<dyn Iterator<Item = &'a library::Track> + 'a> {
            let artist = artist.to_string();
            let album = album.to_string();
            Box::new(self.tracks.iter().filter(move |t| {
                t.album.eq_ignore_ascii_case(&album) && t.artist.eq_ignore_ascii_case(&artist)
            }))
        }
        fn tracks_by_name<'a>(
            &'a self,
            name: &str,
        ) -> Box<dyn Iterator<Item = &'a library::Track> + 'a> {
            let name = name.to_string();
            Box::new(
                self.tracks
                    .iter()
                    .filter(move |t| t.name.eq_ignore_ascii_case(&name)),
            )
        }
        fn track_count(&self) -> usize {
            self.tracks.len()
        }
        fn all_tracks(&self) -> Box<dyn Iterator<Item = &library::Track> + '_> {
            Box::new(self.tracks.iter())
        }
        fn music_folder(&self) -> Option<&str> {
            None
        }
    }

    #[test]
    fn strip_track_number_cases() {
        assert_eq!(strip_track_number("310 Oh Johnny"), "Oh Johnny");
        assert_eq!(strip_track_number("02 To You"), "To You");
        assert_eq!(strip_track_number("Blue Rain"), "Blue Rain"); // no number prefix
        assert_eq!(strip_track_number("A1 Track"), "A1 Track"); // mixed prefix
        assert_eq!(strip_track_number("42"), "42"); // no space
        assert_eq!(strip_track_number(""), "");
    }

    // -- find_matching_tracks --

    fn make_test_library() -> TestLibrary {
        TestLibrary {
            tracks: vec![
                library::Track {
                    id: 1,
                    name: "Creep".into(),
                    artist: "Radiohead".into(),
                    album: "Pablo Honey".into(),
                    ..Default::default()
                },
                library::Track {
                    id: 2,
                    name: "Karma Police".into(),
                    artist: "Radiohead".into(),
                    album: "OK Computer".into(),
                    ..Default::default()
                },
                library::Track {
                    id: 3,
                    name: "Army of Me".into(),
                    artist: "Bjork".into(),
                    album: "Post".into(),
                    ..Default::default()
                },
            ],
        }
    }

    #[test]
    fn find_matching_tracks_all_types() {
        let lib = make_test_library();
        assert_eq!(
            find_matching_tracks(&lib, SyncType::Artist, "Radiohead")
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            find_matching_tracks(&lib, SyncType::Album, "OK Computer")
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            find_matching_tracks(&lib, SyncType::Track, "Creep")
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn find_matching_tracks_artist_not_found_suggests() {
        let lib = make_test_library();
        let err = find_matching_tracks(&lib, SyncType::Artist, "Radio").unwrap_err();
        assert!(err.contains("No tracks found"));
        assert!(err.contains("Radiohead"));
    }

    #[test]
    fn find_matching_tracks_error_cases() {
        let lib = make_test_library();
        assert!(find_matching_tracks(&lib, SyncType::Album, "Nonexistent").is_err());
    }

    #[test]
    fn sync_type_from_str() {
        assert_eq!("artist".parse::<SyncType>().unwrap(), SyncType::Artist);
        assert_eq!("album".parse::<SyncType>().unwrap(), SyncType::Album);
        assert_eq!("track".parse::<SyncType>().unwrap(), SyncType::Track);
        assert!("playlist"
            .parse::<SyncType>()
            .unwrap_err()
            .contains("Unknown sync type"));
        assert!("genre"
            .parse::<SyncType>()
            .unwrap_err()
            .contains("Unknown sync type"));
    }

    // -- collect_music_files --

    #[test]
    fn collect_music_files_filters_and_recurses() {
        let dir = std::env::temp_dir().join("zune-test-collect");
        let _ = std::fs::remove_dir_all(&dir);
        let sub = dir.join("subdir");
        std::fs::create_dir_all(&sub).unwrap();

        std::fs::write(dir.join("song.mp3"), b"fake").unwrap();
        std::fs::write(dir.join("notes.txt"), b"fake").unwrap();
        std::fs::write(sub.join("deep.flac"), b"fake").unwrap();

        let files = collect_music_files(&[dir.to_str().unwrap()]);
        assert_eq!(files.len(), 2);
        assert!(files.iter().any(|f| f.ends_with("song.mp3")));
        assert!(files.iter().any(|f| f.ends_with("deep.flac")));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn collect_music_files_accepts_single_file() {
        let dir = std::env::temp_dir().join("zune-test-collect-single");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("track.wma");
        std::fs::write(&f, b"fake").unwrap();

        assert_eq!(collect_music_files(&[f.to_str().unwrap()]).len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -- collect_photo_files --

    #[test]
    fn collect_photo_files_filters_and_recurses() {
        let dir = std::env::temp_dir().join("zune-test-collect-photos");
        let _ = std::fs::remove_dir_all(&dir);
        let sub = dir.join("subdir");
        std::fs::create_dir_all(&sub).unwrap();

        std::fs::write(dir.join("wallpaper.jpg"), b"fake").unwrap();
        std::fs::write(dir.join("photo.png"), b"fake").unwrap();
        std::fs::write(dir.join("notes.txt"), b"fake").unwrap();
        std::fs::write(dir.join("song.mp3"), b"fake").unwrap();
        std::fs::write(sub.join("deep.bmp"), b"fake").unwrap();

        let files = collect_photo_files(&[dir.to_str().unwrap()]);
        assert_eq!(files.len(), 3);
        assert!(files.iter().any(|f| f.ends_with("wallpaper.jpg")));
        assert!(files.iter().any(|f| f.ends_with("photo.png")));
        assert!(files.iter().any(|f| f.ends_with("deep.bmp")));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn collect_photo_files_accepts_single_file() {
        let dir = std::env::temp_dir().join("zune-test-collect-photo-single");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("pic.jpeg");
        std::fs::write(&f, b"fake").unwrap();

        assert_eq!(collect_photo_files(&[f.to_str().unwrap()]).len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn collect_photo_files_rejects_non_photo() {
        let dir = std::env::temp_dir().join("zune-test-collect-photo-reject");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("song.mp3");
        std::fs::write(&f, b"fake").unwrap();

        assert_eq!(collect_photo_files(&[f.to_str().unwrap()]).len(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -- collect_video_files --

    #[test]
    fn collect_video_files_filters_and_recurses() {
        let dir = std::env::temp_dir().join("zune-test-collect-videos");
        let _ = std::fs::remove_dir_all(&dir);
        let sub = dir.join("subdir");
        std::fs::create_dir_all(&sub).unwrap();

        std::fs::write(dir.join("clip.wmv"), b"fake").unwrap();
        std::fs::write(dir.join("movie.mp4"), b"fake").unwrap();
        std::fs::write(dir.join("notes.txt"), b"fake").unwrap();
        std::fs::write(sub.join("deep.avi"), b"fake").unwrap();

        let files = collect_video_files(&[dir.to_str().unwrap()]);
        assert_eq!(files.len(), 3);
        assert!(files.iter().any(|f| f.ends_with("clip.wmv")));
        assert!(files.iter().any(|f| f.ends_with("movie.mp4")));
        assert!(files.iter().any(|f| f.ends_with("deep.avi")));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn collect_video_files_rejects_non_video() {
        let dir = std::env::temp_dir().join("zune-test-collect-video-reject");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("photo.jpg");
        std::fs::write(&f, b"fake").unwrap();

        assert_eq!(collect_video_files(&[f.to_str().unwrap()]).len(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -- resize_photo_for_zune --

    #[test]
    fn resize_photo_landscape_fits_within_bounds() {
        let dir = std::env::temp_dir().join("zune-test-resize-landscape");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let img_path = dir.join("landscape.png");
        let img: image::ImageBuffer<image::Rgb<u8>, Vec<u8>> = image::ImageBuffer::new(640, 480);
        img.save(&img_path).unwrap();

        let jpeg_data = resize_photo_for_zune(img_path.to_str().unwrap()).unwrap();
        assert!(!jpeg_data.is_empty());

        let resized = image::load_from_memory(&jpeg_data).unwrap();
        assert!(resized.width() <= 240);
        assert!(resized.height() <= 320);
        assert_eq!(resized.width(), 240);
        assert_eq!(resized.height(), 180);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resize_photo_portrait_fits_within_bounds() {
        let dir = std::env::temp_dir().join("zune-test-resize-portrait");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let img_path = dir.join("portrait.png");
        let img: image::ImageBuffer<image::Rgb<u8>, Vec<u8>> = image::ImageBuffer::new(480, 640);
        img.save(&img_path).unwrap();

        let jpeg_data = resize_photo_for_zune(img_path.to_str().unwrap()).unwrap();
        let resized = image::load_from_memory(&jpeg_data).unwrap();
        assert!(resized.width() <= 240);
        assert!(resized.height() <= 320);
        assert_eq!(resized.height(), 320);
        assert_eq!(resized.width(), 240);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resize_photo_produces_valid_jpeg() {
        let dir = std::env::temp_dir().join("zune-test-resize-jpeg");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let img_path = dir.join("test.png");
        let img: image::ImageBuffer<image::Rgb<u8>, Vec<u8>> = image::ImageBuffer::new(100, 100);
        img.save(&img_path).unwrap();

        let jpeg_data = resize_photo_for_zune(img_path.to_str().unwrap()).unwrap();
        assert!(jpeg_data.len() >= 2);
        assert_eq!(jpeg_data[0], 0xFF);
        assert_eq!(jpeg_data[1], 0xD8);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resize_photo_nonexistent_file_errors() {
        assert!(resize_photo_for_zune("/nonexistent/photo.jpg").is_err());
    }

    // -- Sync engine (MockSession) --

    fn make_test_track(
        id: u64,
        name: &str,
        artist: &str,
        album: &str,
        path: &str,
    ) -> library::Track {
        library::Track {
            id,
            name: name.into(),
            artist: artist.into(),
            album: album.into(),
            location: Some(path.into()),
            ..Default::default()
        }
    }

    fn make_device_entry(id: u64, name: &str) -> mtp::parse::DeviceEntry {
        mtp::parse::DeviceEntry {
            object_id: id,
            storage_id: 65537,
            format: "MP3".into(),
            size: 1000,
            name: name.into(),
            ..Default::default()
        }
    }

    struct MockSession {
        import_calls: Vec<String>,
        device_tracks: Vec<mtp::parse::DeviceEntry>,
        next_import_id: u64,
    }

    impl MockSession {
        fn new() -> Self {
            MockSession {
                import_calls: Vec::new(),
                device_tracks: Vec::new(),
                next_import_id: 100,
            }
        }
    }

    impl DeviceSession for MockSession {
        fn ls(&mut self, _path: &str) -> Result<Vec<mtp::parse::DeviceEntry>, mtp::DeviceError> {
            Ok(vec![])
        }
        fn import_track(
            &mut self,
            local_path: &str,
            _meta: Option<&mtp::TrackMeta>,
        ) -> Result<u64, mtp::DeviceError> {
            self.import_calls.push(local_path.to_string());
            let id = self.next_import_id;
            self.next_import_id += 1;
            Ok(id)
        }
        fn rm(&mut self, _device_path: &str) -> Result<(), mtp::DeviceError> {
            Ok(())
        }
        fn rm_by_id(&mut self, _object_id: u32) -> Result<(), mtp::DeviceError> {
            Ok(())
        }
        fn cleanup_empty_folders(&mut self) -> Result<usize, mtp::DeviceError> {
            Ok(0)
        }
        fn get_storage_info(&mut self) -> Result<(u64, u64), mtp::DeviceError> {
            Ok((30_000_000_000, 15_000_000_000))
        }
        fn collect_all_tracks(
            &mut self,
            _path: &str,
        ) -> Result<Vec<mtp::parse::DeviceEntry>, mtp::DeviceError> {
            Ok(self.device_tracks.clone())
        }
    }

    fn setup_test_files(test_name: &str, count: usize) -> (std::path::PathBuf, Vec<String>) {
        let dir = std::env::temp_dir().join(format!("zune-test-{}", test_name));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let files: Vec<String> = (1..=count)
            .map(|i| {
                let f = dir.join(format!("track{i}.mp3"));
                std::fs::write(&f, b"fake mp3 data").unwrap();
                f.to_str().unwrap().to_string()
            })
            .collect();
        (dir, files)
    }

    #[test]
    fn sync_imports_all_tracks() {
        let (dir, files) = setup_test_files("sync-import", 5);
        let temp_dir = dir.join("transcode");

        let t1 = make_test_track(1, "Song 1", "Artist", "Album A", &files[0]);
        let t2 = make_test_track(2, "Song 2", "Artist", "Album A", &files[1]);
        let t3 = make_test_track(3, "Song 3", "Artist", "Album A", &files[2]);
        let t4 = make_test_track(4, "Song 4", "Artist", "Album B", &files[3]);
        let t5 = make_test_track(5, "Song 5", "Artist", "Album B", &files[4]);
        let tracks: Vec<&library::Track> = vec![&t1, &t2, &t3, &t4, &t5];

        let mut mock = MockSession::new();
        let caps = test_caps();
        let result = sync_to_device(&mut mock, &tracks, &temp_dir, &caps).unwrap();

        assert_eq!(result.success, 5);
        assert_eq!(result.skipped, 0);
        assert_eq!(mock.import_calls.len(), 5);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sync_skips_tracks_already_on_device() {
        let (dir, files) = setup_test_files("sync-skip", 3);
        let temp_dir = dir.join("transcode");

        let t1 = make_test_track(1, "Song 1", "Artist", "Album", &files[0]);
        let t2 = make_test_track(2, "Song 2", "Artist", "Album", &files[1]);
        let t3 = make_test_track(3, "Song 3", "Artist", "Album", &files[2]);
        let tracks: Vec<&library::Track> = vec![&t1, &t2, &t3];

        let mut mock = MockSession::new();
        mock.device_tracks
            .push(make_device_entry(50, "Artist/Album/Song 2.mp3"));

        let caps = test_caps();
        let result = sync_to_device(&mut mock, &tracks, &temp_dir, &caps).unwrap();

        assert_eq!(result.success, 2);
        assert_eq!(result.skipped, 1);
        assert_eq!(mock.import_calls.len(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    // -- Sync dedup key tests (artist-based dedup) --

    #[test]
    fn sync_does_not_skip_same_name_different_artist() {
        let (dir, files) = setup_test_files("dedup-artist", 2);
        let temp_dir = dir.join("transcode");

        let t1 = make_test_track(1, "Song", "Artist A", "Album", &files[0]);
        let t2 = make_test_track(2, "Song", "Artist B", "Album", &files[1]);
        let tracks: Vec<&library::Track> = vec![&t1, &t2];

        let mut mock = MockSession::new();
        mock.device_tracks
            .push(make_device_entry(50, "Artist A/Album/Song.mp3"));

        let caps = test_caps();
        let result = sync_to_device(&mut mock, &tracks, &temp_dir, &caps).unwrap();

        assert_eq!(result.success, 1); // Only Artist B's track imported
        assert_eq!(result.skipped, 1); // Artist A's track skipped

        let _ = std::fs::remove_dir_all(&dir);
    }

    // -- strip_track_number edge cases --

    #[test]
    fn strip_track_number_edge_cases() {
        assert_eq!(strip_track_number("01 "), ""); // number + space + empty
        assert_eq!(strip_track_number("1"), "1"); // just a number, no space
    }

    // -- _with_logger seam tests --

    #[test]
    fn collect_photo_files_with_logger_captures_missing_path() {
        // Call collect_photo_files_with_logger against a nonexistent path and
        // verify the "Not found" message routes through the logger rather than
        // going to stderr (which would corrupt a ratatui frame buffer).
        use std::sync::{Arc, Mutex};

        let missing = "/tmp/zytunes-no-such-dir-for-logger-test-photos";
        let _ = std::fs::remove_dir_all(missing);

        let msgs: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let msgs_clone = msgs.clone();
        let log: cache::Logger =
            Arc::new(move |msg: &str| msgs_clone.lock().unwrap().push(msg.to_string()));

        let files = collect_photo_files_with_logger(&[missing], &log);
        assert!(files.is_empty());

        // "Not found" is emitted when a path is neither a file nor a directory.
        let captured = msgs.lock().unwrap();
        assert!(
            captured.iter().any(|m| m.contains("Not found")),
            "expected 'Not found' in logger output; got: {captured:?}"
        );
    }

    #[test]
    #[cfg(unix)]
    fn collect_photo_files_with_logger_captures_unreadable_subdir() {
        // Verify that collect_files_recursive_with_logger routes a read_dir
        // error through the logger rather than stderr.
        use std::os::unix::fs::PermissionsExt;
        use std::sync::{Arc, Mutex};

        let dir = std::env::temp_dir().join("zytunes-test-logger-unreadable-subdir");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("photo.jpg"), b"fake").unwrap();
        let subdir = dir.join("subdir");
        std::fs::create_dir_all(&subdir).unwrap();
        std::fs::set_permissions(&subdir, std::fs::Permissions::from_mode(0o000)).unwrap();

        // Root ignores permission bits, so the unreadable-dir failure path
        // can't be exercised (e.g. CI containers). Skip rather than assert
        // a diagnostic that can't be produced.
        if std::fs::read_dir(&subdir).is_ok() {
            std::fs::set_permissions(&subdir, std::fs::Permissions::from_mode(0o755)).ok();
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }

        let msgs: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let msgs_clone = msgs.clone();
        let log: cache::Logger =
            Arc::new(move |msg: &str| msgs_clone.lock().unwrap().push(msg.to_string()));

        let files = collect_photo_files_with_logger(&[dir.to_str().unwrap()], &log);

        // Restore permissions so cleanup works.
        std::fs::set_permissions(&subdir, std::fs::Permissions::from_mode(0o755)).ok();
        let _ = std::fs::remove_dir_all(&dir);

        // The photo in the parent dir should still be found.
        assert!(
            files.iter().any(|f| f.ends_with("photo.jpg")),
            "expected photo.jpg to be found; got: {files:?}"
        );

        // "Cannot read directory" must come through the logger, not stderr.
        let captured = msgs.lock().unwrap();
        assert!(
            captured.iter().any(|m| m.contains("Cannot read directory")),
            "expected 'Cannot read directory' in logger output; got: {captured:?}"
        );
    }

    #[test]
    fn collect_video_files_with_logger_captures_skip_message() {
        // Pass a non-video file to collect_video_files_with_logger and verify
        // the "Skipping non-video file" message routes through the logger.
        use std::sync::{Arc, Mutex};

        let dir = std::env::temp_dir().join("zytunes-test-logger-skip-video");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("photo.jpg");
        std::fs::write(&f, b"fake").unwrap();

        let msgs: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let msgs_clone = msgs.clone();
        let log: cache::Logger =
            Arc::new(move |msg: &str| msgs_clone.lock().unwrap().push(msg.to_string()));

        let files = collect_video_files_with_logger(&[f.to_str().unwrap()], &log);
        assert!(files.is_empty());

        let captured = msgs.lock().unwrap();
        assert!(
            captured.iter().any(|m| m.contains("Skipping non-video")),
            "expected 'Skipping non-video' in logger output; got: {captured:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
