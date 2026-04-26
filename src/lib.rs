// Self-alias the crate so TUI source files re-included via `#[path]` (gated
// by `tui-testing`) can keep referring to lib items as `zytunes::*` exactly
// like they do when compiled as part of the `zytunes-tui` binary, where the
// lib is a normal external dependency.
#[cfg(feature = "tui-testing")]
extern crate self as zytunes;

pub mod art_cache;
pub mod cache;
pub mod device;
pub mod dirlib;
pub mod fingerprint;
pub mod library;
pub mod local_plays;
pub mod mtp;
pub mod paths;

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
#[path = "tui/ui.rs"]
pub mod ui;

use device::{
    DetectedDevice, DeviceBackend, DeviceCapabilities, DeviceFamily, IpodBackend, ZuneBackend,
};
use library::MusicLibrary;
use mtp::DeviceSession;
use std::fmt;
use std::path::Path;
use std::str::FromStr;

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

/// Formats the Zune 30 natively supports (no transcoding needed).
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

    let mut last_err = String::from("No device backends available");
    for backend in &backends {
        match backend.detect() {
            Ok(detected) => {
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
            Err(e) => {
                last_err = e;
            }
        }
    }

    Err(last_err)
}

/// Create a unique temp directory for transcoded files (includes PID to avoid collisions).
pub fn make_transcode_temp_dir() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("zytunes-transcode-{}", std::process::id()))
}

/// Check if a file needs transcoding for the target device.
pub fn needs_transcoding(path: &str, supported_formats: &[&str]) -> bool {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    !supported_formats.contains(&ext.as_str())
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

/// Check if a video file needs transcoding for the Zune 30 (only WMV is native).
pub fn needs_video_transcoding(path: &str) -> bool {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    ext != "wmv"
}

/// Check whether ffmpeg is available on the system.
pub fn check_ffmpeg_available() -> bool {
    std::process::Command::new("ffmpeg")
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Transcode a video file to WMV format for the Zune 30 via ffmpeg.
///
/// Uses wmv2 video codec at 320x240 and wmav2 audio — the Zune 30's native
/// playback format. Returns the path to the output WMV file.
pub fn transcode_to_wmv(input: &str, temp_dir: &Path) -> Result<String, String> {
    std::fs::create_dir_all(temp_dir).map_err(|e| format!("Cannot create temp dir: {e}"))?;

    let stem = Path::new(input)
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy();
    let output = temp_dir.join(format!("{stem}.wmv"));

    let result = std::process::Command::new("ffmpeg")
        .args([
            "-y",
            "-i",
            input,
            "-c:v",
            "wmv2",
            "-b:v",
            "768k",
            "-vf",
            "scale=320:240:force_original_aspect_ratio=decrease,pad=320:240:(ow-iw)/2:(oh-ih)/2",
            "-c:a",
            "wmav2",
            "-b:a",
            "128k",
            "-ar",
            "44100",
        ])
        .arg(output.to_str().unwrap())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| format!("Failed to run ffmpeg: {e}"))?;

    if !result.status.success() {
        let stderr = String::from_utf8_lossy(&result.stderr);
        return Err(format!("ffmpeg transcode failed: {}", stderr));
    }

    Ok(output.to_string_lossy().into_owned())
}

/// Transcode a video if needed, then import via the device session.
pub fn transcode_and_import_video(
    session: &mut dyn DeviceSession,
    local_path: &str,
    temp_dir: &Path,
) -> Result<u64, String> {
    let upload_path = if needs_video_transcoding(local_path) {
        transcode_to_wmv(local_path, temp_dir)?
    } else {
        local_path.to_string()
    };
    let filename = Path::new(&upload_path)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let data = std::fs::read(&upload_path).map_err(|e| format!("Read video: {e}"))?;
    session.import_video(&filename, &data)
}

/// Transcode if needed, then import via the device session.
pub fn transcode_and_import(
    session: &mut dyn DeviceSession,
    local_path: &str,
    temp_dir: &Path,
    caps: &DeviceCapabilities,
    meta: Option<&mtp::TrackMeta>,
) -> Result<u64, String> {
    let upload_path = if needs_transcoding(local_path, caps.supported_formats) {
        transcode_to_mp3(local_path, temp_dir, caps.max_art_dimensions)?
    } else {
        local_path.to_string()
    };
    session.import_track(&upload_path, meta)
}

/// Byte index of the end of the last frame in `samples` that contains any
/// non-zero value. Returns 0 if the entire slice is digital silence.
fn last_nonzero_frame_end(samples: &[f32], channels: usize) -> usize {
    if channels == 0 {
        return 0;
    }
    let last_nonzero = samples
        .iter()
        .rposition(|&s| s != 0.0)
        .map(|i| i + 1)
        .unwrap_or(0);
    last_nonzero.div_ceil(channels) * channels
}

fn encode_pcm(
    encoder: &mut mp3lame_encoder::Encoder,
    samples: &[f32],
    channels: usize,
    output: &mut Vec<u8>,
) -> Result<(), String> {
    if samples.is_empty() {
        return Ok(());
    }
    let frames = samples.len() / channels;
    output.reserve(mp3lame_encoder::max_required_buffer_size(frames));
    let res = match channels {
        1 => encoder.encode_to_vec(mp3lame_encoder::MonoPcm(samples), output),
        2 => encoder.encode_to_vec(mp3lame_encoder::InterleavedPcm(samples), output),
        n => return Err(format!("Unsupported channel count: {n}")),
    };
    res.map(|_| ()).map_err(|e| format!("LAME encode: {e:?}"))
}

/// Transcode a file to MP3 using pure Rust libraries.
/// Preserves metadata and resizes album art to the given dimensions (or 200x200 by default).
pub fn transcode_to_mp3(
    input: &str,
    temp_dir: &Path,
    max_art_dimensions: Option<(u32, u32)>,
) -> Result<String, String> {
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

    if channels == 0 || channels > 2 {
        return Err(format!(
            "Cannot transcode {channels}-channel audio; only mono and stereo are supported"
        ));
    }

    let mut mp3_data = Vec::new();
    let mut sample_buf: Option<SampleBuffer<f32>> = None;
    // Trailing run of zero samples. Held rather than encoded so that digital
    // silence at the tail of the stream is dropped at EOF. symphonia's isomp4
    // demuxer parses M4A edit-list atoms but does not apply them, so silence
    // the source's edit list would have trimmed otherwise bleeds into LAME.
    let mut trailing_zeros: Vec<f32> = Vec::new();
    let mut encoded_any = false;

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
        let split_at = last_nonzero_frame_end(samples, channels);
        let (audio, trailing) = samples.split_at(split_at);

        if !audio.is_empty() {
            if !trailing_zeros.is_empty() {
                encode_pcm(&mut mp3_encoder, &trailing_zeros, channels, &mut mp3_data)?;
                trailing_zeros.clear();
            }
            encode_pcm(&mut mp3_encoder, audio, channels, &mut mp3_data)?;
            encoded_any = true;
        }

        trailing_zeros.extend_from_slice(trailing);
    }

    if encoded_any {
        // Held trailing zeros were genuinely at the tail — drop them.
        drop(trailing_zeros);
    } else {
        // Entire stream was digital silence. Preserve it so downstream tag
        // writing and LAME's flush both have valid state to work with.
        encode_pcm(&mut mp3_encoder, &trailing_zeros, channels, &mut mp3_data)?;
    }

    // FlushGap is the correct end-of-stream flush for a standalone track
    // (pads the final frame with zeros, allows id3v1). FlushNoGap is for
    // gapless concatenation and leaves the final frame padded with ancillary
    // data, which some decoders (including the Zune) mishandle.
    mp3_encoder
        .flush_to_vec::<mp3lame_encoder::FlushGap>(&mut mp3_data)
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

    // Resize and embed album art (dimensions from device capabilities, default 200x200)
    if let Some(art_data) = meta.album_art {
        if let Ok(img) = image::load_from_memory(&art_data) {
            let (art_w, art_h) = max_art_dimensions.unwrap_or((200, 200));
            let resized = img.resize_exact(art_w, art_h, image::imageops::FilterType::Lanczos3);
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
    let mut files = Vec::new();

    for path in paths {
        let p = Path::new(path);
        if p.is_file() {
            if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
                if music_extensions.contains(&ext.to_lowercase().as_str()) {
                    files.push(path.to_string());
                } else {
                    log(&format!("Skipping non-music file: {path}"));
                }
            }
        } else if p.is_dir() {
            collect_files_recursive_with_logger(p, &music_extensions, &mut files, log);
        } else {
            log(&format!("Not found: {path}"));
        }
    }

    files.sort();
    files
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
    let mut files = Vec::new();

    for path in paths {
        let p = Path::new(path);
        if p.is_file() {
            if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
                if photo_extensions.contains(&ext.to_lowercase().as_str()) {
                    files.push(path.to_string());
                } else {
                    log(&format!("Skipping non-photo file: {path}"));
                }
            }
        } else if p.is_dir() {
            collect_files_recursive_with_logger(p, &photo_extensions, &mut files, log);
        } else {
            log(&format!("Not found: {path}"));
        }
    }

    files.sort();
    files
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
    let mut files = Vec::new();

    for path in paths {
        let p = Path::new(path);
        if p.is_file() {
            if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
                if video_extensions.contains(&ext.to_lowercase().as_str()) {
                    files.push(path.to_string());
                } else {
                    log(&format!("Skipping non-video file: {path}"));
                }
            }
        } else if p.is_dir() {
            collect_files_recursive_with_logger(p, &video_extensions, &mut files, log);
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

/// Resize a photo to fit within the Zune 30 screen (240x320) and encode as JPEG.
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
    use id3::TagLike;

    const ZUNE_FORMATS: &[&str] = &["mp3", "wma", "aac"];

    fn test_caps() -> DeviceCapabilities {
        DeviceCapabilities {
            family: DeviceFamily::Zune,
            supported_formats: ZUNE_FORMATS,
            transcode_target: "mp3",
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
    fn needs_transcoding_by_extension() {
        // Native formats — no transcoding
        assert!(!needs_transcoding("song.mp3", ZUNE_FORMATS));
        assert!(!needs_transcoding("song.wma", ZUNE_FORMATS));
        assert!(!needs_transcoding("song.aac", ZUNE_FORMATS));
        // Non-native — needs transcoding
        assert!(needs_transcoding("song.flac", ZUNE_FORMATS));
        assert!(needs_transcoding("song.m4a", ZUNE_FORMATS));
        assert!(needs_transcoding("song.ogg", ZUNE_FORMATS));
        assert!(needs_transcoding("song.wav", ZUNE_FORMATS));
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

    // -- needs_video_transcoding --

    #[test]
    fn wmv_does_not_need_transcoding() {
        assert!(!needs_video_transcoding("movie.wmv"));
        assert!(!needs_video_transcoding("MOVIE.WMV"));
        assert!(!needs_video_transcoding("/path/to/file.Wmv"));
    }

    #[test]
    fn non_wmv_needs_transcoding() {
        assert!(needs_video_transcoding("movie.mp4"));
        assert!(needs_video_transcoding("movie.avi"));
        assert!(needs_video_transcoding("movie.mpeg"));
        assert!(needs_video_transcoding("movie.mpg"));
        assert!(needs_video_transcoding("movie.mkv"));
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
        fn ls(&mut self, _path: &str) -> Result<Vec<mtp::parse::DeviceEntry>, String> {
            Ok(vec![])
        }
        fn import_track(
            &mut self,
            local_path: &str,
            _meta: Option<&mtp::TrackMeta>,
        ) -> Result<u64, String> {
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

    // -- Transcoding decision tests --

    #[test]
    fn needs_transcoding_native_formats() {
        assert!(!needs_transcoding("song.mp3", ZUNE_FORMATS));
        assert!(!needs_transcoding("song.wma", ZUNE_FORMATS));
        assert!(!needs_transcoding("song.aac", ZUNE_FORMATS));
        assert!(!needs_transcoding("SONG.MP3", ZUNE_FORMATS)); // case insensitive
    }

    #[test]
    fn needs_transcoding_non_native_formats() {
        assert!(needs_transcoding("song.flac", ZUNE_FORMATS));
        assert!(needs_transcoding("song.ogg", ZUNE_FORMATS));
        assert!(needs_transcoding("song.wav", ZUNE_FORMATS));
        assert!(needs_transcoding("song.m4a", ZUNE_FORMATS));
        assert!(needs_transcoding("song.opus", ZUNE_FORMATS));
        assert!(needs_transcoding("song.alac", ZUNE_FORMATS));
        assert!(needs_transcoding("song.aiff", ZUNE_FORMATS));
    }

    #[test]
    fn needs_transcoding_no_extension() {
        assert!(needs_transcoding("noext", ZUNE_FORMATS));
        assert!(needs_transcoding("", ZUNE_FORMATS));
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

    /// WAV with `audio_frames` of low-amplitude alternating samples followed
    /// by `silence_frames` of exact zeros (stereo s16le @ 44.1 kHz).
    fn make_wav_tail(path: &std::path::Path, audio_frames: usize, silence_frames: usize) {
        use std::io::Write;
        let channels: u16 = 2;
        let sample_rate: u32 = 44100;
        let bits_per_sample: u16 = 16;
        let byte_rate = sample_rate * u32::from(channels) * u32::from(bits_per_sample) / 8;
        let block_align = channels * bits_per_sample / 8;
        let total_frames = audio_frames + silence_frames;
        let data_size =
            (total_frames * usize::from(channels) * usize::from(bits_per_sample) / 8) as u32;
        let file_size = 36 + data_size;

        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(b"RIFF").unwrap();
        f.write_all(&file_size.to_le_bytes()).unwrap();
        f.write_all(b"WAVE").unwrap();
        f.write_all(b"fmt ").unwrap();
        f.write_all(&16u32.to_le_bytes()).unwrap();
        f.write_all(&1u16.to_le_bytes()).unwrap();
        f.write_all(&channels.to_le_bytes()).unwrap();
        f.write_all(&sample_rate.to_le_bytes()).unwrap();
        f.write_all(&byte_rate.to_le_bytes()).unwrap();
        f.write_all(&block_align.to_le_bytes()).unwrap();
        f.write_all(&bits_per_sample.to_le_bytes()).unwrap();
        f.write_all(b"data").unwrap();
        f.write_all(&data_size.to_le_bytes()).unwrap();

        for i in 0..audio_frames {
            let v: i16 = if i % 2 == 0 { 2000 } else { -2000 };
            let bytes = v.to_le_bytes();
            for _ in 0..channels {
                f.write_all(&bytes).unwrap();
            }
        }
        let silence = vec![0u8; silence_frames * usize::from(channels) * 2];
        f.write_all(&silence).unwrap();
    }

    fn count_mp3_frames(mp3_path: &str) -> u64 {
        use symphonia::core::codecs::DecoderOptions;
        use symphonia::core::formats::FormatOptions;
        use symphonia::core::io::MediaSourceStream;
        use symphonia::core::meta::MetadataOptions;
        use symphonia::core::probe::Hint;

        let file = std::fs::File::open(mp3_path).unwrap();
        let mss = MediaSourceStream::new(Box::new(file), Default::default());
        let mut hint = Hint::new();
        hint.with_extension("mp3");
        let probed = symphonia::default::get_probe()
            .format(
                &hint,
                mss,
                &FormatOptions::default(),
                &MetadataOptions::default(),
            )
            .unwrap();
        let mut format = probed.format;
        let track = format.default_track().unwrap().clone();
        let mut decoder = symphonia::default::get_codecs()
            .make(&track.codec_params, &DecoderOptions::default())
            .unwrap();

        let mut total: u64 = 0;
        loop {
            let packet = match format.next_packet() {
                Ok(p) => p,
                Err(symphonia::core::errors::Error::IoError(ref e))
                    if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    break;
                }
                Err(_) => break,
            };
            if packet.track_id() != track.id {
                continue;
            }
            let decoded = decoder.decode(&packet).unwrap();
            total += decoded.frames() as u64;
        }
        total
    }

    #[test]
    fn last_nonzero_frame_end_finds_tail() {
        // Stereo: [L0 R0 L1 R1 L2 R2]
        let samples = [0.1f32, 0.2, 0.3, 0.4, 0.0, 0.0];
        assert_eq!(last_nonzero_frame_end(&samples, 2), 4);

        let all_zero = [0.0f32; 8];
        assert_eq!(last_nonzero_frame_end(&all_zero, 2), 0);

        // Non-zero only in the right channel of the last frame — still a
        // frame with content, must round up to include both samples.
        let right_only = [0.0f32, 0.0, 0.0, 0.5];
        assert_eq!(last_nonzero_frame_end(&right_only, 2), 4);

        // Non-zero in one sample of the middle frame; trailing frame is zero.
        let mid_only = [0.0f32, 0.0, 0.1, 0.0, 0.0, 0.0];
        assert_eq!(last_nonzero_frame_end(&mid_only, 2), 4);

        // Mono
        let mono = [0.0f32, 0.5, 0.0];
        assert_eq!(last_nonzero_frame_end(&mono, 1), 2);
    }

    #[test]
    fn transcode_trims_trailing_silence() {
        let dir = std::env::temp_dir().join("zytunes-test-transcode-trim");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let wav_path = dir.join("tail.wav");
        // 1s of audio + 7s of digital silence (the slowerpace pattern).
        make_wav_tail(&wav_path, 44_100, 44_100 * 7);

        let out_dir = dir.join("out");
        std::fs::create_dir_all(&out_dir).unwrap();

        let mp3_path =
            transcode_to_mp3(wav_path.to_str().unwrap(), &out_dir, Some((200, 200))).unwrap();

        let frames = count_mp3_frames(&mp3_path);

        // Without the trim, output would be ~8s (~352,800 frames). With the
        // trim it should be ~1s plus a small LAME delay — comfortably under
        // 2s. Guard against a regression where the 7s tail leaks back in.
        assert!(
            frames < 44_100 * 2,
            "output still contains trailing silence: {} frames (~{:.2}s)",
            frames,
            frames as f64 / 44_100.0
        );
        // Sanity check: we didn't strip the actual audio.
        assert!(
            frames > 22_050,
            "output suspiciously short: {} frames (~{:.3}s)",
            frames,
            frames as f64 / 44_100.0
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn transcode_preserves_mid_track_silence() {
        // A gap of zeros between two regions of audio should survive.
        let dir = std::env::temp_dir().join("zytunes-test-transcode-gap");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let wav_path = dir.join("gap.wav");
        // Build: 1s audio, 2s silence, 1s audio. We write it manually so the
        // trailing audio forces the held-zero buffer to flush.
        {
            use std::io::Write;
            let channels: u16 = 2;
            let sample_rate: u32 = 44100;
            let bits_per_sample: u16 = 16;
            let audio_frames = 44_100usize;
            let silence_frames = 44_100usize * 2;
            let total_frames = audio_frames * 2 + silence_frames;
            let byte_rate = sample_rate * u32::from(channels) * u32::from(bits_per_sample) / 8;
            let block_align = channels * bits_per_sample / 8;
            let data_size =
                (total_frames * usize::from(channels) * usize::from(bits_per_sample) / 8) as u32;
            let file_size = 36 + data_size;

            let mut f = std::fs::File::create(&wav_path).unwrap();
            f.write_all(b"RIFF").unwrap();
            f.write_all(&file_size.to_le_bytes()).unwrap();
            f.write_all(b"WAVE").unwrap();
            f.write_all(b"fmt ").unwrap();
            f.write_all(&16u32.to_le_bytes()).unwrap();
            f.write_all(&1u16.to_le_bytes()).unwrap();
            f.write_all(&channels.to_le_bytes()).unwrap();
            f.write_all(&sample_rate.to_le_bytes()).unwrap();
            f.write_all(&byte_rate.to_le_bytes()).unwrap();
            f.write_all(&block_align.to_le_bytes()).unwrap();
            f.write_all(&bits_per_sample.to_le_bytes()).unwrap();
            f.write_all(b"data").unwrap();
            f.write_all(&data_size.to_le_bytes()).unwrap();

            for i in 0..audio_frames {
                let v: i16 = if i % 2 == 0 { 2000 } else { -2000 };
                let bytes = v.to_le_bytes();
                for _ in 0..channels {
                    f.write_all(&bytes).unwrap();
                }
            }
            let silence = vec![0u8; silence_frames * usize::from(channels) * 2];
            f.write_all(&silence).unwrap();
            for i in 0..audio_frames {
                let v: i16 = if i % 2 == 0 { 2000 } else { -2000 };
                let bytes = v.to_le_bytes();
                for _ in 0..channels {
                    f.write_all(&bytes).unwrap();
                }
            }
        }

        let out_dir = dir.join("out");
        std::fs::create_dir_all(&out_dir).unwrap();

        let mp3_path =
            transcode_to_mp3(wav_path.to_str().unwrap(), &out_dir, Some((200, 200))).unwrap();

        let frames = count_mp3_frames(&mp3_path);
        // Total source = 4s. Output should keep the middle gap (mid-track
        // silence is not trailing), so frames ~= 4s worth. Allow slack for
        // LAME delay/padding on either side.
        assert!(
            frames > 44_100 * 3,
            "mid-track silence was incorrectly trimmed: {} frames (~{:.2}s)",
            frames,
            frames as f64 / 44_100.0
        );

        let _ = std::fs::remove_dir_all(&dir);
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

        let result = transcode_to_mp3(wav_path.to_str().unwrap(), &out_dir, Some((200, 200)));
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

        let mp3_path =
            transcode_to_mp3(wav_path.to_str().unwrap(), &out_dir, Some((200, 200))).unwrap();

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

        let mp3_path =
            transcode_to_mp3(wav_path.to_str().unwrap(), &out_dir, Some((200, 200))).unwrap();

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

        let result = transcode_to_mp3(wav_path.to_str().unwrap(), &out_dir, Some((200, 200)));
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

        let result = transcode_to_mp3("/nonexistent/path/song.flac", &out_dir, Some((200, 200)));
        assert!(result.is_err());

        let _ = std::fs::remove_dir_all(&out_dir);
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
