pub mod device;
pub mod dirlib;
pub mod library;
pub mod mtp;

use device::ZuneDevice;
use library::MusicLibrary;
use mtp::{DeviceSession, NativeSession};
use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::str::FromStr;

/// The type of sync operation to perform.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SyncType {
    Artist,
    Album,
    Playlist,
    Track,
}

impl FromStr for SyncType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "artist" => Ok(SyncType::Artist),
            "album" => Ok(SyncType::Album),
            "playlist" => Ok(SyncType::Playlist),
            "track" => Ok(SyncType::Track),
            other => Err(format!(
                "Unknown sync type: \"{other}\". Use: artist, album, playlist, track"
            )),
        }
    }
}

impl fmt::Display for SyncType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SyncType::Artist => write!(f, "artist"),
            SyncType::Album => write!(f, "album"),
            SyncType::Playlist => write!(f, "playlist"),
            SyncType::Track => write!(f, "track"),
        }
    }
}

/// Resolve the iTunes library path: `ZYTUNES_LIBRARY` env var, or `$HOME/Music/Music/Library.xml`.
pub fn library_xml_path() -> String {
    if let Ok(p) = std::env::var("ZYTUNES_LIBRARY") {
        return p;
    }
    if let Ok(home) = std::env::var("HOME") {
        return format!("{home}/Music/Music/Library.xml");
    }
    "/Music/Music/Library.xml".to_string()
}

/// Load a music library with auto-detection.
///
/// Tries sources in order:
/// 1. iTunes Library.xml at `xml_path` (if the file exists)
/// 2. `ZYTUNES_MUSIC_DIR` environment variable (directory scan)
/// 3. `music_dir` from config.toml (directory scan)
///
/// Returns a boxed trait object so callers are backend-agnostic.
pub fn load_library(
    xml_path: &str,
    music_dir: Option<&str>,
) -> Result<Box<dyn MusicLibrary + Send>, String> {
    // 1. Try iTunes XML if the file exists.
    if Path::new(xml_path).exists() {
        return library::ItunesLibrary::parse(xml_path)
            .map(|l| Box::new(l) as Box<dyn MusicLibrary + Send>);
    }

    // 2. Try ZYTUNES_MUSIC_DIR env var.
    if let Ok(dir) = std::env::var("ZYTUNES_MUSIC_DIR") {
        if Path::new(&dir).is_dir() {
            return dirlib::DirectoryLibrary::scan(&dir)
                .map(|l| Box::new(l) as Box<dyn MusicLibrary + Send>);
        }
    }

    // 3. Try music_dir from config.
    if let Some(dir) = music_dir {
        if Path::new(dir).is_dir() {
            return dirlib::DirectoryLibrary::scan(dir)
                .map(|l| Box::new(l) as Box<dyn MusicLibrary + Send>);
        }
    }

    Err(
        "No music library found. Set ZYTUNES_LIBRARY to an iTunes XML path, \
         or ZYTUNES_MUSIC_DIR to a music folder, \
         or add music_dir to ~/.config/zytunes/config.toml"
            .into(),
    )
}

/// Formats the Zune 30 natively supports (no transcoding needed).
pub const ZUNE_NATIVE_FORMATS: &[&str] = &["mp3", "wma", "aac"];

/// Connect to the Zune and return an active session.
pub fn connect() -> Result<NativeSession, String> {
    let zune = ZuneDevice::find().map_err(|e| format!("{}", e))?;
    println!(
        "Zune detected: {}",
        zune.product_name.as_deref().unwrap_or("Zune")
    );

    print!("Connecting (MTPZ handshake)... ");
    let log = |msg: &str| {
        eprintln!("{}", msg);
    };
    match NativeSession::open(zune.product_id, &log) {
        Ok(mut s) => {
            s.set_serial(zune.serial_number);
            println!("OK");
            Ok(s)
        }
        Err(e) => {
            println!("FAILED");
            Err(e)
        }
    }
}

/// Create a unique temp directory for transcoded files (includes PID to avoid collisions).
pub fn make_transcode_temp_dir() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("zytunes-transcode-{}", std::process::id()))
}

/// Check if a file needs transcoding for the Zune.
pub fn needs_transcoding(path: &str) -> bool {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    !ZUNE_NATIVE_FORMATS.contains(&ext.as_str())
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

/// Transcode if needed, then import via the device session.
pub fn transcode_and_import(
    session: &mut dyn DeviceSession,
    local_path: &str,
    temp_dir: &Path,
) -> Result<u64, String> {
    let upload_path = if needs_transcoding(local_path) {
        transcode_to_mp3(local_path, temp_dir)?
    } else {
        local_path.to_string()
    };
    session.zune_import(&upload_path)
}

/// Transcode a file to MP3 using pure Rust libraries.
/// Preserves metadata and resizes album art to 200x200 (Zune rejects larger art with 0xa803).
pub fn transcode_to_mp3(input: &str, temp_dir: &Path) -> Result<String, String> {
    use id3::TagLike;
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::DecoderOptions;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let input_path = Path::new(input);
    let stem = input_path.file_stem().unwrap_or_default().to_string_lossy();
    let output = temp_dir.join(format!("{stem}.mp3"));

    // 1. Read metadata and album art with lofty
    let meta = read_lofty_metadata(input)?;

    // 2. Decode audio with symphonia
    let file = std::fs::File::open(input).map_err(|e| format!("Cannot open {input}: {e}"))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = input_path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| format!("Failed to probe {input}: {e}"))?;

    let mut format = probed.format;
    let track = format
        .default_track()
        .ok_or("No audio track found")?
        .clone();

    let sample_rate = track
        .codec_params
        .sample_rate
        .ok_or("Unknown sample rate")?;
    let channels = track.codec_params.channels.map(|c| c.count()).unwrap_or(2);

    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| format!("Failed to create decoder: {e}"))?;

    // 3. Encode to MP3 with LAME
    let mut mp3_builder = mp3lame_encoder::Builder::new().ok_or("Failed to create LAME builder")?;
    mp3_builder
        .set_num_channels(channels as u8)
        .map_err(|e| format!("LAME set channels: {e:?}"))?;
    mp3_builder
        .set_sample_rate(sample_rate)
        .map_err(|e| format!("LAME set sample rate: {e:?}"))?;
    mp3_builder
        .set_vbr_mode(mp3lame_encoder::VbrMode::Mtrh)
        .map_err(|e| format!("LAME set VBR mode: {e:?}"))?;
    mp3_builder
        .set_vbr_quality(mp3lame_encoder::Quality::NearBest)
        .map_err(|e| format!("LAME set VBR quality: {e:?}"))?;
    let mut mp3_encoder = mp3_builder
        .build()
        .map_err(|e| format!("LAME build: {e:?}"))?;

    let mut mp3_data = Vec::new();
    let mut sample_buf: Option<SampleBuffer<f32>> = None;

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(symphonia::core::errors::Error::IoError(ref e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(e) => return Err(format!("Decode error: {e}")),
        };

        if packet.track_id() != track.id {
            continue;
        }

        let decoded = decoder
            .decode(&packet)
            .map_err(|e| format!("Decode packet: {e}"))?;

        let spec = *decoded.spec();
        let duration = decoded.capacity() as u64;

        if sample_buf.is_none() {
            sample_buf = Some(SampleBuffer::new(duration, spec));
        }
        let sbuf = sample_buf.as_mut().unwrap();
        sbuf.copy_interleaved_ref(decoded);

        let samples = sbuf.samples();
        let input_pcm = mp3lame_encoder::InterleavedPcm(samples);
        mp3_data.reserve(mp3lame_encoder::max_required_buffer_size(
            samples.len() / channels,
        ));
        mp3_encoder
            .encode_to_vec(input_pcm, &mut mp3_data)
            .map_err(|e| format!("LAME encode: {e:?}"))?;
    }

    // Flush the encoder
    mp3_encoder
        .flush_to_vec::<mp3lame_encoder::FlushNoGap>(&mut mp3_data)
        .map_err(|e| format!("LAME flush: {e:?}"))?;

    // 4. Write the MP3 file
    std::fs::write(&output, &mp3_data).map_err(|e| format!("Write MP3: {e}"))?;

    // 5. Write ID3v2.3 tags and album art
    let mut tag = id3::Tag::new();
    if !meta.artist.is_empty() {
        tag.set_artist(&meta.artist);
    }
    if !meta.album.is_empty() {
        tag.set_album(&meta.album);
    }
    if !meta.title.is_empty() {
        tag.set_title(&meta.title);
    }
    if meta.track_num > 0 {
        tag.set_track(meta.track_num);
    }
    if !meta.genre.is_empty() {
        tag.set_genre(&meta.genre);
    }

    // Resize and embed album art (200x200 JPEG for Zune)
    if let Some(art_data) = meta.album_art {
        if let Ok(img) = image::load_from_memory(&art_data) {
            let resized = img.resize_exact(200, 200, image::imageops::FilterType::Lanczos3);
            let mut jpeg_buf = std::io::Cursor::new(Vec::new());
            if resized
                .write_to(&mut jpeg_buf, image::ImageFormat::Jpeg)
                .is_ok()
            {
                tag.add_frame(id3::frame::Picture {
                    mime_type: "image/jpeg".to_string(),
                    picture_type: id3::frame::PictureType::CoverFront,
                    description: String::new(),
                    data: jpeg_buf.into_inner(),
                });
            }
        }
    }

    tag.write_to_path(&output, id3::Version::Id3v23)
        .map_err(|e| format!("Write ID3 tags: {e}"))?;

    Ok(output.to_string_lossy().to_string())
}

/// Metadata extracted from an audio file for transcoding.
struct AudioMetadata {
    artist: String,
    album: String,
    title: String,
    track_num: u32,
    genre: String,
    album_art: Option<Vec<u8>>,
}

/// Read metadata and album art from an audio file using lofty.
fn read_lofty_metadata(path: &str) -> Result<AudioMetadata, String> {
    use lofty::file::TaggedFileExt;
    use lofty::tag::Accessor;

    let tagged = lofty::probe::read_from_path(path)
        .map_err(|e| format!("Failed to read metadata from {path}: {e}"))?;

    let tag = match tagged.primary_tag().or_else(|| tagged.first_tag()) {
        Some(t) => t,
        None => {
            let stem = Path::new(path)
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            return Ok(AudioMetadata {
                artist: String::new(),
                album: String::new(),
                title: stem,
                track_num: 0,
                genre: String::new(),
                album_art: None,
            });
        }
    };

    Ok(AudioMetadata {
        artist: tag.artist().map(|s| s.to_string()).unwrap_or_default(),
        album: tag.album().map(|s| s.to_string()).unwrap_or_default(),
        title: tag.title().map(|s| s.to_string()).unwrap_or_else(|| {
            Path::new(path)
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string()
        }),
        track_num: tag.track().unwrap_or(0),
        genre: tag.genre().map(|s| s.to_string()).unwrap_or_default(),
        album_art: tag.pictures().first().map(|pic| pic.data().to_vec()),
    })
}

/// Find matching tracks from the library for the given sync type and name.
pub fn find_matching_tracks<'a>(
    lib: &'a dyn MusicLibrary,
    sync_type: SyncType,
    name: &str,
) -> Result<Vec<&'a library::Track>, String> {
    match sync_type {
        SyncType::Artist => {
            let tracks = lib.artist_tracks(name);
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
            let tracks = lib.album_tracks(name);
            if tracks.is_empty() {
                return Err(format!("No tracks found for album \"{}\"", name));
            }
            println!("Album \"{}\": {} tracks", name, tracks.len());
            Ok(tracks)
        }
        SyncType::Playlist => {
            let tracks = lib.playlist_tracks(name);
            if tracks.is_empty() {
                let mut msg = format!("No tracks found for playlist \"{}\"", name);
                let playlists = lib.user_playlists();
                if !playlists.is_empty() {
                    msg.push_str("\nAvailable playlists:");
                    for p in &playlists {
                        msg.push_str(&format!("\n  {} ({} tracks)", p.name, p.track_ids.len()));
                    }
                }
                return Err(msg);
            }
            println!("Playlist \"{}\": {} tracks", name, tracks.len());
            Ok(tracks)
        }
        SyncType::Track => {
            let tracks = lib.tracks_by_name(name);
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

/// Sync tracks to the device: scan for duplicates, import new tracks, optionally create playlist.
pub fn sync_to_device(
    session: &mut dyn DeviceSession,
    pushable: &[&library::Track],
    sync_type: SyncType,
    name: &str,
    temp_dir: &Path,
) -> Result<SyncResult, String> {
    // Scan device for existing tracks to avoid duplicates.
    print!("Scanning device for existing tracks... ");
    let existing_tracks = session.collect_all_tracks("/Music").unwrap_or_default();
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
    let mut imported_ids: HashMap<String, u64> = HashMap::new();

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

        match transcode_and_import(session, loc, temp_dir) {
            Ok(object_id) => {
                println!("  OK (id: {})", object_id);
                imported_ids.insert(track.name.to_lowercase(), object_id);
                success += 1;
            }
            Err(e) => {
                println!("  FAILED: {}", e);
                failed += 1;
            }
        }
    }

    // For playlist syncs, create a playlist on the device.
    if sync_type == SyncType::Playlist {
        create_device_playlist(session, name, pushable, &imported_ids, &existing_tracks)?;
    }

    Ok(SyncResult {
        success,
        failed,
        skipped,
    })
}

/// Create a playlist on the device, looking up IDs for skipped (already-present) tracks.
pub fn create_device_playlist(
    session: &mut dyn DeviceSession,
    name: &str,
    pushable: &[&library::Track],
    imported_ids: &HashMap<String, u64>,
    existing_device_tracks: &[mtp::parse::DeviceEntry],
) -> Result<(), String> {
    println!("\nCreating playlist \"{}\" on device...", name);

    // For tracks that were already on device (skipped), find their IDs.
    let skipped_tracks: Vec<&&library::Track> = pushable
        .iter()
        .filter(|t| {
            let key = t.name.to_lowercase();
            !imported_ids.contains_key(&key)
                || pushable
                    .iter()
                    .filter(|t2| t2.name.to_lowercase() == key)
                    .count()
                    > 1
        })
        .collect();

    let mut all_ids = imported_ids.clone();

    if !skipped_tracks.is_empty() {
        print!(
            "  Looking up {} skipped tracks on device... ",
            skipped_tracks.len()
        );
        // Re-scan only if new tracks were imported (existing list may be stale).
        let fresh_tracks;
        let device_tracks = if imported_ids.is_empty() {
            existing_device_tracks
        } else {
            fresh_tracks = session.collect_all_tracks("/Music").unwrap_or_default();
            &fresh_tracks
        };
        let mut used_ids: std::collections::HashSet<u64> = all_ids.values().copied().collect();

        for track in &skipped_tracks {
            if all_ids.contains_key(&track.name.to_lowercase()) {
                continue;
            }
            let meta_key = track.name.to_lowercase();
            let file_key = track.location.as_ref().and_then(|loc| {
                Path::new(loc)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map(|s| strip_track_number(s).to_lowercase())
            });

            let found = device_tracks.iter().find(|dt| {
                if used_ids.contains(&dt.object_id) {
                    return false;
                }
                let stem = dt.name.rsplit('/').next().unwrap_or(&dt.name);
                let stem = stem.rsplit('.').next_back().unwrap_or(stem);
                let stem = strip_track_number(stem);
                let device_key = stem.to_lowercase();
                device_key == meta_key || file_key.as_ref().is_some_and(|fk| device_key == *fk)
            });
            if let Some(dt) = found {
                used_ids.insert(dt.object_id);
                let unique_key = format!("{}_{}", meta_key, dt.object_id);
                all_ids.insert(unique_key, dt.object_id);
            }
        }
        println!("done");
    }

    // Build the final playlist track ID list, one per pushable track.
    let mut playlist_track_ids: Vec<u64> = Vec::new();
    let mut used_ids: std::collections::HashSet<u64> = std::collections::HashSet::new();
    for track in pushable {
        let key = track.name.to_lowercase();
        // First try exact match from all_ids.
        if let Some(&id) = all_ids.get(&key) {
            if used_ids.insert(id) {
                playlist_track_ids.push(id);
                continue;
            }
        }
        // Try uniquified keys (for duplicates found via device scan).
        let found = all_ids
            .iter()
            .find(|(k, &id)| k.starts_with(&key) && !used_ids.contains(&id))
            .map(|(_, &id)| id);
        if let Some(id) = found {
            used_ids.insert(id);
            playlist_track_ids.push(id);
        } else {
            eprintln!("  Warning: no device match for \"{}\"", track.name);
        }
    }

    println!(
        "  {} of {} tracks matched",
        playlist_track_ids.len(),
        pushable.len()
    );

    if playlist_track_ids.is_empty() {
        eprintln!("  Warning: no matching tracks found on device");
    } else {
        session.create_playlist(name, &playlist_track_ids)?;
        println!(
            "  Playlist \"{}\" created with {} tracks",
            name,
            playlist_track_ids.len()
        );
    }

    Ok(())
}

/// Expand paths into a list of music files.
/// If a path is a directory, recursively find music files in it.
pub fn collect_music_files(paths: &[&str]) -> Vec<String> {
    let music_extensions = [
        "mp3", "wma", "aac", "m4a", "ogg", "flac", "wav", "opus", "alac", "aiff",
    ];
    let mut files = Vec::new();

    for path in paths {
        let p = Path::new(path);
        if p.is_file() {
            if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
                if music_extensions.contains(&ext.to_lowercase().as_str()) {
                    files.push(path.to_string());
                } else {
                    eprintln!("Skipping non-music file: {}", path);
                }
            }
        } else if p.is_dir() {
            collect_music_files_recursive(p, &music_extensions, &mut files);
        } else {
            eprintln!("Not found: {}", path);
        }
    }

    files.sort();
    files
}

pub fn collect_music_files_recursive(dir: &Path, extensions: &[&str], files: &mut Vec<String>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("Cannot read directory {}: {}", dir.display(), e);
            return;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_music_files_recursive(&path, extensions, files);
        } else if path.is_file() {
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if extensions.contains(&ext.to_lowercase().as_str()) {
                    files.push(path.to_string_lossy().to_string());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::ItunesLibrary;
    use id3::TagLike;

    #[test]
    fn needs_transcoding_by_extension() {
        // Native formats — no transcoding
        assert!(!needs_transcoding("song.mp3"));
        assert!(!needs_transcoding("song.wma"));
        assert!(!needs_transcoding("song.aac"));
        // Non-native — needs transcoding
        assert!(needs_transcoding("song.flac"));
        assert!(needs_transcoding("song.m4a"));
        assert!(needs_transcoding("song.ogg"));
        assert!(needs_transcoding("song.wav"));
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

    #[test]
    fn temp_dir_includes_pid() {
        let dir = make_transcode_temp_dir();
        assert!(dir
            .to_str()
            .unwrap()
            .contains(&std::process::id().to_string()));
    }

    // -- find_matching_tracks --

    fn make_test_library() -> ItunesLibrary {
        let mut tracks = std::collections::HashMap::new();
        tracks.insert(
            1,
            library::Track {
                id: 1,
                name: "Creep".into(),
                artist: "Radiohead".into(),
                album: "Pablo Honey".into(),
                album_artist: None,
                genre: None,
                year: None,
                track_number: None,
                disc_number: None,
                total_time_ms: None,
                location: None,
                kind: None,
            },
        );
        tracks.insert(
            2,
            library::Track {
                id: 2,
                name: "Karma Police".into(),
                artist: "Radiohead".into(),
                album: "OK Computer".into(),
                album_artist: None,
                genre: None,
                year: None,
                track_number: None,
                disc_number: None,
                total_time_ms: None,
                location: None,
                kind: None,
            },
        );
        tracks.insert(
            3,
            library::Track {
                id: 3,
                name: "Army of Me".into(),
                artist: "Bjork".into(),
                album: "Post".into(),
                album_artist: None,
                genre: None,
                year: None,
                track_number: None,
                disc_number: None,
                total_time_ms: None,
                location: None,
                kind: None,
            },
        );
        ItunesLibrary {
            tracks,
            playlists: vec![library::Playlist {
                name: "Road Trip".into(),
                track_ids: vec![1, 3],
            }],
            music_folder_path: None,
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
            find_matching_tracks(&lib, SyncType::Playlist, "Road Trip")
                .unwrap()
                .len(),
            2
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
        assert_eq!("playlist".parse::<SyncType>().unwrap(), SyncType::Playlist);
        assert_eq!("track".parse::<SyncType>().unwrap(), SyncType::Track);
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
            album_artist: None,
            genre: None,
            year: None,
            track_number: None,
            disc_number: None,
            total_time_ms: None,
            location: Some(path.into()),
            kind: None,
        }
    }

    fn make_device_entry(id: u64, name: &str) -> mtp::parse::DeviceEntry {
        mtp::parse::DeviceEntry {
            object_id: id,
            storage_id: 65537,
            format: "MP3".into(),
            size: 1000,
            name: name.into(),
        }
    }

    struct MockSession {
        import_calls: Vec<String>,
        playlist_calls: Vec<(String, Vec<u64>)>,
        device_tracks: Vec<mtp::parse::DeviceEntry>,
        next_import_id: u64,
    }

    impl MockSession {
        fn new() -> Self {
            MockSession {
                import_calls: Vec::new(),
                playlist_calls: Vec::new(),
                device_tracks: Vec::new(),
                next_import_id: 100,
            }
        }
    }

    impl DeviceSession for MockSession {
        fn ls(&mut self, _path: &str) -> Result<Vec<mtp::parse::DeviceEntry>, String> {
            Ok(vec![])
        }
        fn zune_import(&mut self, local_path: &str) -> Result<u64, String> {
            self.import_calls.push(local_path.to_string());
            let id = self.next_import_id;
            self.next_import_id += 1;
            Ok(id)
        }
        fn rm(&mut self, _device_path: &str) -> Result<(), String> {
            Ok(())
        }
        fn rm_by_id(&mut self, _object_id: u32) -> Result<(), String> {
            Ok(())
        }
        fn cleanup_empty_folders(&mut self) -> Result<usize, String> {
            Ok(0)
        }
        fn get_storage_info(&mut self) -> Result<(u64, u64), String> {
            Ok((30_000_000_000, 15_000_000_000))
        }
        fn collect_all_tracks(
            &mut self,
            _path: &str,
        ) -> Result<Vec<mtp::parse::DeviceEntry>, String> {
            Ok(self.device_tracks.clone())
        }
        fn create_playlist(&mut self, name: &str, track_ids: &[u64]) -> Result<(), String> {
            self.playlist_calls
                .push((name.to_string(), track_ids.to_vec()));
            Ok(())
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
        let result =
            sync_to_device(&mut mock, &tracks, SyncType::Artist, "Artist", &temp_dir).unwrap();

        assert_eq!(result.success, 5);
        assert_eq!(result.skipped, 0);
        assert_eq!(mock.import_calls.len(), 5);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sync_playlist_creates_playlist_with_correct_ids() {
        let (dir, files) = setup_test_files("sync-playlist", 4);
        let temp_dir = dir.join("transcode");

        let t1 = make_test_track(1, "Song 1", "Artist A", "Album X", &files[0]);
        let t2 = make_test_track(2, "Song 2", "Artist A", "Album X", &files[1]);
        let t3 = make_test_track(3, "Song 3", "Artist B", "Album Y", &files[2]);
        let t4 = make_test_track(4, "Song 4", "Artist B", "Album Y", &files[3]);
        let tracks: Vec<&library::Track> = vec![&t1, &t2, &t3, &t4];

        let mut mock = MockSession::new();
        let result = sync_to_device(
            &mut mock,
            &tracks,
            SyncType::Playlist,
            "Road Trip",
            &temp_dir,
        )
        .unwrap();

        assert_eq!(result.success, 4);
        assert_eq!(mock.playlist_calls.len(), 1);
        assert_eq!(mock.playlist_calls[0].0, "Road Trip");
        assert_eq!(mock.playlist_calls[0].1, vec![100, 101, 102, 103]);

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

        let result =
            sync_to_device(&mut mock, &tracks, SyncType::Artist, "Artist", &temp_dir).unwrap();

        assert_eq!(result.success, 2);
        assert_eq!(result.skipped, 1);
        assert_eq!(mock.import_calls.len(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    // -- Transcoding decision tests --

    #[test]
    fn needs_transcoding_native_formats() {
        assert!(!needs_transcoding("song.mp3"));
        assert!(!needs_transcoding("song.wma"));
        assert!(!needs_transcoding("song.aac"));
        assert!(!needs_transcoding("SONG.MP3")); // case insensitive
    }

    #[test]
    fn needs_transcoding_non_native_formats() {
        assert!(needs_transcoding("song.flac"));
        assert!(needs_transcoding("song.ogg"));
        assert!(needs_transcoding("song.wav"));
        assert!(needs_transcoding("song.m4a"));
        assert!(needs_transcoding("song.opus"));
        assert!(needs_transcoding("song.alac"));
        assert!(needs_transcoding("song.aiff"));
    }

    #[test]
    fn needs_transcoding_no_extension() {
        assert!(needs_transcoding("noext"));
        assert!(needs_transcoding(""));
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

        let result =
            sync_to_device(&mut mock, &tracks, SyncType::Artist, "Test", &temp_dir).unwrap();

        assert_eq!(result.success, 1); // Only Artist B's track imported
        assert_eq!(result.skipped, 1); // Artist A's track skipped

        let _ = std::fs::remove_dir_all(&dir);
    }

    // -- Library path resolution test --

    #[test]
    fn library_xml_path_uses_env_var() {
        let original = std::env::var("ZYTUNES_LIBRARY").ok();
        std::env::set_var("ZYTUNES_LIBRARY", "/custom/path.xml");
        assert_eq!(library_xml_path(), "/custom/path.xml");
        match original {
            Some(v) => std::env::set_var("ZYTUNES_LIBRARY", v),
            None => std::env::remove_var("ZYTUNES_LIBRARY"),
        }
    }

    // -- strip_track_number edge cases --

    #[test]
    fn strip_track_number_edge_cases() {
        assert_eq!(strip_track_number("01 "), ""); // number + space + empty
        assert_eq!(strip_track_number("1"), "1"); // just a number, no space
    }

    // -- transcode_to_mp3 tests --

    /// Generate a minimal valid WAV file (PCM s16le, stereo, 44100 Hz).
    fn make_wav(path: &std::path::Path, num_samples: usize) {
        use std::io::Write;
        let channels: u16 = 2;
        let sample_rate: u32 = 44100;
        let bits_per_sample: u16 = 16;
        let byte_rate = sample_rate * u32::from(channels) * u32::from(bits_per_sample) / 8;
        let block_align = channels * bits_per_sample / 8;
        let data_size =
            (num_samples * usize::from(channels) * usize::from(bits_per_sample) / 8) as u32;
        let file_size = 36 + data_size;

        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(b"RIFF").unwrap();
        f.write_all(&file_size.to_le_bytes()).unwrap();
        f.write_all(b"WAVE").unwrap();
        f.write_all(b"fmt ").unwrap();
        f.write_all(&16u32.to_le_bytes()).unwrap(); // chunk size
        f.write_all(&1u16.to_le_bytes()).unwrap(); // PCM
        f.write_all(&channels.to_le_bytes()).unwrap();
        f.write_all(&sample_rate.to_le_bytes()).unwrap();
        f.write_all(&byte_rate.to_le_bytes()).unwrap();
        f.write_all(&block_align.to_le_bytes()).unwrap();
        f.write_all(&bits_per_sample.to_le_bytes()).unwrap();
        f.write_all(b"data").unwrap();
        f.write_all(&data_size.to_le_bytes()).unwrap();
        // Write silence (zeros)
        let silence = vec![0u8; data_size as usize];
        f.write_all(&silence).unwrap();
    }

    /// Generate a minimal JPEG image of given dimensions.
    fn make_jpeg(width: u32, height: u32) -> Vec<u8> {
        use image::{ImageBuffer, Rgb};
        let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::new(width, height);
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Jpeg).unwrap();
        buf.into_inner()
    }

    #[test]
    fn transcode_wav_to_mp3() {
        let dir = std::env::temp_dir().join("zytunes-test-transcode-wav");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let wav_path = dir.join("silence.wav");
        make_wav(&wav_path, 44100); // 1 second of stereo silence

        let out_dir = dir.join("out");
        std::fs::create_dir_all(&out_dir).unwrap();

        let result = transcode_to_mp3(wav_path.to_str().unwrap(), &out_dir);
        assert!(result.is_ok(), "transcode failed: {:?}", result.err());

        let mp3_path = result.unwrap();
        assert!(mp3_path.ends_with(".mp3"));
        assert!(std::path::Path::new(&mp3_path).exists());

        // Verify it's a valid MP3 (starts with ID3 header or MPEG sync)
        let data = std::fs::read(&mp3_path).unwrap();
        assert!(!data.is_empty());
        let has_id3 = data.starts_with(b"ID3");
        let has_sync = data.len() >= 2 && data[0] == 0xff && (data[1] & 0xe0) == 0xe0;
        assert!(has_id3 || has_sync, "output is not a valid MP3 file");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn transcode_preserves_metadata() {
        let dir = std::env::temp_dir().join("zytunes-test-transcode-meta");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let wav_path = dir.join("tagged.wav");
        make_wav(&wav_path, 44100);

        // Write metadata to the WAV using lofty
        {
            use lofty::file::TaggedFileExt;
            use lofty::tag::{Accessor, Tag, TagExt};
            let mut tagged = lofty::probe::read_from_path(&wav_path).unwrap();
            let tag_type = tagged.primary_tag_type();
            // Insert a tag if none exists (WAV files may not have one)
            if tagged.primary_tag().is_none() {
                tagged.insert_tag(Tag::new(tag_type));
            }
            let tag = tagged.primary_tag_mut().unwrap();
            tag.set_artist("Test Artist".to_string());
            tag.set_album("Test Album".to_string());
            tag.set_title("Test Title".to_string());
            tag.set_track(7);
            tag.set_genre("Rock".to_string());
            tag.save_to_path(&wav_path, lofty::config::WriteOptions::default())
                .unwrap();
        }

        let out_dir = dir.join("out");
        std::fs::create_dir_all(&out_dir).unwrap();

        let mp3_path = transcode_to_mp3(wav_path.to_str().unwrap(), &out_dir).unwrap();

        // Read back the ID3 tags from the output MP3
        let tag = id3::Tag::read_from_path(&mp3_path).unwrap();
        assert_eq!(tag.artist(), Some("Test Artist"));
        assert_eq!(tag.album(), Some("Test Album"));
        assert_eq!(tag.title(), Some("Test Title"));
        assert_eq!(tag.track(), Some(7));
        assert_eq!(tag.genre_parsed().as_deref(), Some("Rock"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn transcode_resizes_album_art() {
        let dir = std::env::temp_dir().join("zytunes-test-transcode-art");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let wav_path = dir.join("art.wav");
        make_wav(&wav_path, 44100);

        // Embed a 500x500 JPEG as album art
        let big_art = make_jpeg(500, 500);
        {
            use lofty::file::TaggedFileExt;
            use lofty::picture::{MimeType, Picture, PictureType};
            use lofty::tag::{Accessor, Tag, TagExt};
            let mut tagged = lofty::probe::read_from_path(&wav_path).unwrap();
            let tag_type = tagged.primary_tag_type();
            if tagged.primary_tag().is_none() {
                tagged.insert_tag(Tag::new(tag_type));
            }
            let tag = tagged.primary_tag_mut().unwrap();
            tag.set_artist("Art Artist".to_string());
            let pic = Picture::new_unchecked(
                PictureType::CoverFront,
                Some(MimeType::Jpeg),
                None,
                big_art,
            );
            tag.push_picture(pic);
            tag.save_to_path(&wav_path, lofty::config::WriteOptions::default())
                .unwrap();
        }

        let out_dir = dir.join("out");
        std::fs::create_dir_all(&out_dir).unwrap();

        let mp3_path = transcode_to_mp3(wav_path.to_str().unwrap(), &out_dir).unwrap();

        // Read the embedded art from the output MP3 and check dimensions
        let tag = id3::Tag::read_from_path(&mp3_path).unwrap();
        let pics: Vec<_> = tag.pictures().collect();
        assert!(!pics.is_empty(), "no album art in output MP3");
        let art_data = &pics[0].data;
        let img = image::load_from_memory(art_data).unwrap();
        assert_eq!(img.width(), 200);
        assert_eq!(img.height(), 200);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn transcode_no_art_still_works() {
        let dir = std::env::temp_dir().join("zytunes-test-transcode-noart");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let wav_path = dir.join("noart.wav");
        make_wav(&wav_path, 44100);

        let out_dir = dir.join("out");
        std::fs::create_dir_all(&out_dir).unwrap();

        let result = transcode_to_mp3(wav_path.to_str().unwrap(), &out_dir);
        assert!(result.is_ok(), "transcode failed: {:?}", result.err());

        // Verify no art embedded
        let mp3_path = result.unwrap();
        let tag = id3::Tag::read_from_path(&mp3_path);
        if let Ok(tag) = tag {
            assert_eq!(tag.pictures().count(), 0);
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn transcode_nonexistent_file_errors() {
        let out_dir = std::env::temp_dir().join("zytunes-test-transcode-nofile");
        let _ = std::fs::remove_dir_all(&out_dir);
        std::fs::create_dir_all(&out_dir).unwrap();

        let result = transcode_to_mp3("/nonexistent/path/song.flac", &out_dir);
        assert!(result.is_err());

        let _ = std::fs::remove_dir_all(&out_dir);
    }
}
