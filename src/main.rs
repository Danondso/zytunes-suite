mod device;
mod library;
mod mtp;

use device::ZuneDevice;
use library::ItunesLibrary;
use mtp::{AftSession, DeviceSession};
use std::collections::HashMap;
use std::env;
use std::path::Path;
use std::process::Command;

fn main() {
    let args: Vec<String> = env::args().collect();
    if let Err(e) = run(&args) {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

fn run(args: &[String]) -> Result<(), String> {
    let command = args.get(1).map(|s| s.as_str()).unwrap_or("ls");

    match command {
        "ls" => cmd_ls(args.get(2).map(|s| s.as_str()).unwrap_or("/")),
        "push" => {
            if args.len() < 3 {
                return Err("Usage: zytunes push <file-or-directory> [file2 ...]".into());
            }
            cmd_push(&args[2..])
        }
        "rm" => {
            if args.len() < 3 {
                return Err("Usage: zytunes rm <device-path> [path2 ...]".into());
            }
            cmd_rm(&args[2..])
        }
        "library" => {
            let default = default_library_xml();
            let xml_path = args
                .get(2)
                .map(|s| s.as_str())
                .unwrap_or(&default);
            cmd_library(xml_path, args.get(3).map(|s| s.as_str()))
        }
        "sync" => {
            if args.len() < 3 {
                return Err("Usage: zytunes sync <type> <name> [--library <path>]\n  \
                     type: artist, album, playlist, track\n  \
                     e.g.: zytunes sync artist \"Radiohead\"\n       \
                     zytunes sync playlist \"Classic Rock\""
                    .into());
            }
            cmd_sync(&args[2..])
        }
        "help" | "--help" | "-h" => {
            println!("zytunes v0.2.0 — sync music to a Zune 30\n");
            println!("Usage: zytunes <command> [args...]\n");
            println!("Commands:");
            println!("  ls [path]              List device contents (default: /)");
            println!("  push <files...>        Push music files to the Zune");
            println!("  rm <device-paths...>   Remove files/folders from the Zune");
            println!("  sync <type> <name>     Sync from iTunes library to Zune");
            println!("  library [xml] [query]  Browse iTunes library");
            println!("  help                   Show this help");
            println!("\nSync types:");
            println!("  sync artist <name>     Sync all tracks by an artist");
            println!("  sync album <name>      Sync all tracks in an album");
            println!("  sync playlist <name>   Sync all tracks in a playlist");
            println!("  sync track <name>      Sync a single track by name");
            println!("\nUnsupported formats (FLAC, OGG, WAV, M4A, OPUS, etc.)");
            println!("are auto-transcoded to MP3 with album art via ffmpeg.");
            println!("\nExamples:");
            println!("  zytunes sync artist \"Radiohead\"");
            println!("  zytunes sync playlist \"Classic Rock\"");
            println!("  zytunes sync album \"OK Computer\"");
            println!("  zytunes push song.mp3");
            println!("  zytunes ls /Music");
            println!("  zytunes rm \"/Music/Artist/Album\"");
            Ok(())
        }
        other => Err(format!(
            "Unknown command: {other}\nRun 'zytunes help' for usage."
        )),
    }
}

/// Browse the iTunes library.
fn cmd_library(xml_path: &str, query: Option<&str>) -> Result<(), String> {
    println!("Parsing iTunes library: {xml_path}");
    let start = std::time::Instant::now();
    let lib = ItunesLibrary::parse(xml_path)?;
    println!(
        "Loaded {} tracks, {} playlists in {:.1}s\n",
        lib.tracks.len(),
        lib.playlists.len(),
        start.elapsed().as_secs_f64()
    );

    if let Some(music_folder) = &lib.music_folder {
        println!("Music folder: {music_folder}");
    }

    match query {
        Some("artists") => {
            let artists = lib.artists();
            println!("\n{} artists:", artists.len());
            for a in &artists {
                println!("  {a}");
            }
        }
        Some("playlists") => {
            let playlists = lib.user_playlists();
            println!("\n{} playlists:", playlists.len());
            for p in &playlists {
                println!("  {} ({} tracks)", p.name, p.track_ids.len());
            }
        }
        Some("stats") | None => {
            let artists = lib.artists();
            let albums = lib.albums();
            let playlists = lib.user_playlists();
            println!("\nStats:");
            println!("  {} tracks", lib.tracks.len());
            println!("  {} artists", artists.len());
            println!("  {} albums", albums.len());
            println!("  {} playlists", playlists.len());

            // Format breakdown.
            let mut formats: HashMap<String, usize> = HashMap::new();
            for t in lib.tracks.values() {
                let kind = t.kind.as_deref().unwrap_or("Unknown");
                *formats.entry(kind.to_string()).or_default() += 1;
            }
            println!("\nFormats:");
            let mut fmts: Vec<_> = formats.iter().collect();
            fmts.sort_by(|a, b| b.1.cmp(a.1));
            for (kind, count) in fmts {
                println!("  {count:>6}  {kind}");
            }

            if !playlists.is_empty() {
                println!("\nPlaylists:");
                for p in &playlists {
                    println!("  {} ({} tracks)", p.name, p.track_ids.len());
                }
            }
        }
        Some(q) => {
            // Search by artist or playlist name.
            let tracks = lib.artist_tracks(q);
            if !tracks.is_empty() {
                println!("Artist \"{q}\": {} tracks", tracks.len());
                for t in &tracks {
                    println!(
                        "  {} - {} ({})",
                        t.album,
                        t.name,
                        t.kind.as_deref().unwrap_or("?")
                    );
                }
            } else {
                let tracks = lib.playlist_tracks(q);
                if !tracks.is_empty() {
                    println!("Playlist \"{q}\": {} tracks", tracks.len());
                    for t in &tracks {
                        println!("  {} - {} - {}", t.artist, t.album, t.name);
                    }
                } else {
                    println!("No artist or playlist matching \"{q}\"");
                }
            }
        }
    }
    Ok(())
}

/// Default iTunes library path.
/// Override with ZYTUNES_LIBRARY env var.
fn default_library_xml() -> String {
    if let Ok(path) = std::env::var("ZYTUNES_LIBRARY") {
        return path;
    }
    // macOS Music app default location
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    format!("{home}/Music/Music/Library.xml")
}

/// Formats the Zune 30 natively supports (no transcoding needed).
const ZUNE_NATIVE_FORMATS: &[&str] = &["mp3", "wma", "aac"];

/// Connect to the Zune and return an active session.
fn connect() -> Result<AftSession, String> {
    match ZuneDevice::find() {
        Ok(z) => {
            println!(
                "Zune detected: {}",
                z.product_name.as_deref().unwrap_or("Zune")
            );
        }
        Err(e) => return Err(format!("{}", e)),
    };

    print!("Connecting (MTPZ handshake)... ");
    match AftSession::open() {
        Ok(s) => {
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
fn make_transcode_temp_dir() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("zytunes-transcode-{}", std::process::id()))
}

/// Check if a file needs transcoding for the Zune.
fn needs_transcoding(path: &str) -> bool {
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
fn strip_track_number(s: &str) -> &str {
    match s.split_once(' ') {
        Some((prefix, rest)) if prefix.chars().all(|c| c.is_ascii_digit()) => rest.trim(),
        _ => s,
    }
}

/// Transcode if needed, then import via the device session.
fn transcode_and_import(
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

/// Transcode a file to MP3 using ffmpeg.
/// Preserves metadata and resizes album art to 200x200 (Zune rejects larger art with 0xa803).
fn transcode_to_mp3(input: &str, temp_dir: &Path) -> Result<String, String> {
    let input_path = Path::new(input);
    let stem = input_path.file_stem().unwrap_or_default().to_string_lossy();
    let output = temp_dir.join(format!("{stem}.mp3"));

    print!("  Transcoding to MP3... ");

    // Check if the source has embedded art.
    let has_art = Command::new("ffprobe")
        .args([
            "-v",
            "quiet",
            "-select_streams",
            "v",
            "-show_entries",
            "stream=codec_type",
            input,
        ])
        .output()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);

    let mut args = vec![
        "-i".to_string(),
        input.to_string(),
        "-codec:a".to_string(),
        "libmp3lame".to_string(),
        "-q:a".to_string(),
        "2".to_string(),
        "-map_metadata".to_string(),
        "0".to_string(),
    ];

    if has_art {
        // Resize album art to 200x200 JPEG (Zune rejects larger art).
        args.extend([
            "-vf".into(),
            "scale=200:200".into(),
            "-codec:v".into(),
            "mjpeg".into(),
            "-q:v".into(),
            "5".into(),
        ]);
    } else {
        args.push("-vn".into());
    }

    args.extend([
        "-id3v2_version".into(),
        "3".into(),
        "-y".into(),
        output.to_str().unwrap().into(),
    ]);

    let result = Command::new("ffmpeg")
        .args(&args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|e| format!("ffmpeg failed to start: {e}"))?;

    if !result.success() {
        return Err("ffmpeg transcode failed".to_string());
    }

    let size = std::fs::metadata(&output).map(|m| m.len()).unwrap_or(0);
    println!("OK ({:.1} MB)", size as f64 / 1_048_576.0);

    Ok(output.to_string_lossy().to_string())
}

/// Find matching tracks from the library for the given sync type and name.
fn find_matching_tracks<'a>(
    lib: &'a ItunesLibrary,
    sync_type: &str,
    name: &str,
) -> Result<Vec<&'a library::Track>, String> {
    match sync_type {
        "artist" => {
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
        "album" => {
            let tracks: Vec<&library::Track> = lib
                .tracks
                .values()
                .filter(|t| t.album.eq_ignore_ascii_case(name))
                .collect();
            if tracks.is_empty() {
                return Err(format!("No tracks found for album \"{}\"", name));
            }
            println!("Album \"{}\": {} tracks", name, tracks.len());
            Ok(tracks)
        }
        "playlist" => {
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
        "track" => {
            let tracks: Vec<&library::Track> = lib
                .tracks
                .values()
                .filter(|t| t.name.eq_ignore_ascii_case(name))
                .collect();
            if tracks.is_empty() {
                return Err(format!("No track found matching \"{}\"", name));
            }
            println!("Track \"{}\": {} match(es)", name, tracks.len());
            Ok(tracks)
        }
        other => Err(format!(
            "Unknown sync type: \"{other}\". Use: artist, album, playlist, track"
        )),
    }
}

/// Result of a sync operation.
struct SyncResult {
    success: usize,
    failed: usize,
    skipped: usize,
}

/// Sync tracks to the device: scan for duplicates, import new tracks, optionally create playlist.
fn sync_to_device(
    session: &mut dyn DeviceSession,
    pushable: &[&library::Track],
    sync_type: &str,
    name: &str,
    temp_dir: &Path,
) -> Result<SyncResult, String> {
    // Scan device for existing tracks to avoid duplicates.
    print!("Scanning device for existing tracks... ");
    let existing_tracks = session.collect_all_tracks("/Music").unwrap_or_default();
    let existing_names: std::collections::HashSet<String> = existing_tracks
        .iter()
        .map(|t| {
            let name = t.name.rsplit('/').next().unwrap_or(&t.name);
            let stem = name.rsplit('.').next_back().unwrap_or(name);
            strip_track_number(stem).to_lowercase()
        })
        .collect();
    println!("{} tracks on device", existing_tracks.len());

    // Filter out tracks already on the device.
    let mut to_push: Vec<&library::Track> = Vec::new();
    let mut skipped = 0;
    for track in pushable {
        let key = track.name.to_lowercase();
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
        let loc = track.location.as_deref().unwrap();
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
    if sync_type == "playlist" {
        create_device_playlist(session, name, pushable, &imported_ids)?;
    }

    Ok(SyncResult {
        success,
        failed,
        skipped,
    })
}

/// Create a playlist on the device, looking up IDs for skipped (already-present) tracks.
fn create_device_playlist(
    session: &mut dyn DeviceSession,
    name: &str,
    pushable: &[&library::Track],
    imported_ids: &HashMap<String, u64>,
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
        let device_tracks = session.collect_all_tracks("/Music").unwrap_or_default();
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

/// Sync tracks from iTunes library to Zune.
fn cmd_sync(args: &[String]) -> Result<(), String> {
    if args.len() < 2 {
        return Err("Usage: zytunes sync <type> <name> [--library <path>]".into());
    }

    let sync_type = args[0].as_str();
    let name = &args[1];

    // Check for --library flag.
    let xml_path = args
        .iter()
        .position(|a| a == "--library")
        .and_then(|i| args.get(i + 1))
        .map(|s| s.to_string())
        .unwrap_or_else(default_library_xml);
    let xml_path = xml_path.as_str();

    // Parse iTunes library.
    println!("Loading iTunes library...");
    let start = std::time::Instant::now();
    let lib = ItunesLibrary::parse(xml_path)?;
    println!(
        "Loaded {} tracks in {:.1}s\n",
        lib.tracks.len(),
        start.elapsed().as_secs_f64()
    );

    // Find matching tracks.
    let tracks = find_matching_tracks(&lib, sync_type, name)?;

    // Filter to tracks that have a file location.
    let pushable: Vec<&library::Track> = tracks
        .iter()
        .filter(|t| {
            t.location
                .as_ref()
                .is_some_and(|loc| Path::new(loc).exists())
        })
        .copied()
        .collect();

    let missing = tracks.len() - pushable.len();
    if missing > 0 {
        println!("  ({} tracks skipped — files not found on disk)", missing);
    }
    if pushable.is_empty() {
        return Err("No tracks with accessible files to sync.".into());
    }

    // Check which need transcoding.
    let to_transcode = pushable
        .iter()
        .filter(|t| {
            t.location
                .as_ref()
                .is_some_and(|loc| needs_transcoding(loc))
        })
        .count();
    println!(
        "\n{} tracks to push ({} need transcoding)",
        pushable.len(),
        to_transcode
    );

    if to_transcode > 0 && Command::new("ffmpeg").arg("-version").output().is_err() {
        return Err("ffmpeg is required for transcoding. Install with: brew install ffmpeg".into());
    }

    // Connect to Zune.
    let mut session = connect()?;
    println!();

    // Create temp dir for transcoded files.
    let temp_dir = make_transcode_temp_dir();
    if to_transcode > 0 {
        std::fs::create_dir_all(&temp_dir)
            .map_err(|e| format!("Failed to create temp directory: {e}"))?;
    }

    let result = sync_to_device(&mut session, &pushable, sync_type, name, &temp_dir)?;

    // Clean up temp files.
    let _ = std::fs::remove_dir_all(&temp_dir);

    println!(
        "\n{} synced, {} skipped, {} failed out of {} total",
        result.success,
        result.skipped,
        result.failed,
        pushable.len()
    );

    Ok(())
}

/// List device contents at the given path.
fn cmd_ls(path: &str) -> Result<(), String> {
    let mut session = connect()?;
    println!();

    let entries = session.ls(path)?;
    if entries.is_empty() {
        println!("(empty)");
    }
    for entry in &entries {
        let kind = if entry.is_dir() { "DIR " } else { "FILE" };
        let size = if entry.is_dir() {
            String::new()
        } else {
            format!("  ({} bytes)", entry.size)
        };
        println!("  [{}] {}{}", kind, entry.name, size);
    }
    println!("\n{} entries", entries.len());

    Ok(())
}

/// Push music files to the Zune.
/// Automatically transcodes unsupported formats (FLAC, OGG, etc.) to MP3.
fn cmd_push(paths: &[String]) -> Result<(), String> {
    let files = collect_music_files(&paths.iter().map(|s| s.as_str()).collect::<Vec<_>>());
    if files.is_empty() {
        return Err("No music files found.".into());
    }

    // Check which files need transcoding.
    let needs_transcode = files.iter().filter(|f| needs_transcoding(f)).count();

    println!("Found {} music file(s) to push", files.len());
    if needs_transcode > 0 {
        println!(
            "  {} file(s) will be transcoded to MP3 (Zune doesn't support FLAC/OGG/etc.)",
            needs_transcode
        );
        // Verify ffmpeg is available.
        if Command::new("ffmpeg").arg("-version").output().is_err() {
            return Err("ffmpeg is required for transcoding but was not found.\n\
                 Install with: brew install ffmpeg"
                .into());
        }
    }
    println!();

    let mut session = connect()?;
    println!();

    // Create temp dir for transcoded files.
    let temp_dir = make_transcode_temp_dir();
    if needs_transcode > 0 {
        std::fs::create_dir_all(&temp_dir)
            .map_err(|e| format!("Failed to create temp directory: {e}"))?;
    }

    let mut success = 0;
    let mut failed = 0;
    let total = files.len();

    for (i, file) in files.iter().enumerate() {
        let filename = Path::new(file)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        println!("[{}/{}] {}", i + 1, total, filename);

        match transcode_and_import(&mut session, file, &temp_dir) {
            Ok(_id) => {
                println!("  OK");
                success += 1;
            }
            Err(e) => {
                println!("  FAILED: {}", e);
                failed += 1;
            }
        }
    }

    // Clean up temp files.
    let _ = std::fs::remove_dir_all(&temp_dir);

    println!("\nDone: {} uploaded, {} failed", success, failed);
    Ok(())
}

/// Remove files/folders from the Zune.
fn cmd_rm(paths: &[String]) -> Result<(), String> {
    let mut session = connect()?;
    println!();

    for path in paths {
        print!("Removing {}... ", path);
        match session.rm(path) {
            Ok(()) => println!("OK"),
            Err(e) => {
                println!("FAILED");
                eprintln!("  {}", e);
            }
        }
    }

    Ok(())
}

/// Expand paths into a list of music files.
/// If a path is a directory, recursively find music files in it.
fn collect_music_files(paths: &[&str]) -> Vec<String> {
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

fn collect_music_files_recursive(dir: &Path, extensions: &[&str], files: &mut Vec<String>) {
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

    // -- CLI dispatch --

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn run_rejects_missing_args() {
        assert!(run(&args(&["zytunes", "bogus"])).is_err());
        assert!(run(&args(&["zytunes", "push"])).is_err());
        assert!(run(&args(&["zytunes", "rm"])).is_err());
        assert!(run(&args(&["zytunes", "sync"])).is_err());
        assert!(run(&args(&["zytunes", "help"])).is_ok());
    }

    // -- Pure utility functions --

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
            music_folder: None,
        }
    }

    #[test]
    fn find_matching_tracks_all_types() {
        let lib = make_test_library();
        assert_eq!(
            find_matching_tracks(&lib, "artist", "Radiohead")
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            find_matching_tracks(&lib, "album", "OK Computer")
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            find_matching_tracks(&lib, "playlist", "Road Trip")
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            find_matching_tracks(&lib, "track", "Creep").unwrap().len(),
            1
        );
    }

    #[test]
    fn find_matching_tracks_artist_not_found_suggests() {
        let lib = make_test_library();
        let err = find_matching_tracks(&lib, "artist", "Radio").unwrap_err();
        assert!(err.contains("No tracks found"));
        assert!(err.contains("Radiohead"));
    }

    #[test]
    fn find_matching_tracks_error_cases() {
        let lib = make_test_library();
        assert!(find_matching_tracks(&lib, "album", "Nonexistent").is_err());
        assert!(find_matching_tracks(&lib, "genre", "Rock")
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
        let result = sync_to_device(&mut mock, &tracks, "artist", "Artist", &temp_dir).unwrap();

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
        let result =
            sync_to_device(&mut mock, &tracks, "playlist", "Road Trip", &temp_dir).unwrap();

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
        mock.device_tracks.push(make_device_entry(50, "Song 2.mp3"));

        let result = sync_to_device(&mut mock, &tracks, "artist", "Artist", &temp_dir).unwrap();

        assert_eq!(result.success, 2);
        assert_eq!(result.skipped, 1);
        assert_eq!(mock.import_calls.len(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
