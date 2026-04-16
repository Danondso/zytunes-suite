use zytunes::{
    check_ffmpeg_available, collect_music_files, collect_photo_files, collect_video_files, connect,
    find_matching_tracks, make_transcode_temp_dir, needs_transcoding, needs_video_transcoding,
    resize_photo_for_zune, sync_to_device, transcode_and_import, transcode_and_import_video,
};

use std::collections::HashMap;
use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let Err(e) = run(&args) {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

/// Load a field from the TOML config file at ~/.config/zytunes/config.toml.
fn load_config_field(field: &str) -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let path = Path::new(&home)
        .join(".config")
        .join("zytunes")
        .join("config.toml");
    let contents = std::fs::read_to_string(path).ok()?;
    let table: toml::Table = contents.parse().ok()?;
    table.get(field)?.as_str().map(|s| s.to_string())
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
        "library" => cmd_library(args.get(2).map(|s| s.as_str())),
        "sync" => {
            if args.len() < 3 {
                return Err("Usage: zytunes sync <type> <name>\n  \
                     type: artist, album, track\n  \
                     e.g.: zytunes sync artist \"Radiohead\""
                    .into());
            }
            cmd_sync(&args[2..])
        }
        "photo-sync" => {
            let dir = args
                .get(2)
                .map(|s| s.to_string())
                .or_else(|| std::env::var("ZYTUNES_PHOTOS_DIR").ok())
                .or_else(|| load_config_field("photo_dir"))
                .ok_or("No photo directory configured.\n\
                    Set ZYTUNES_PHOTOS_DIR env var, add photo_dir to ~/.config/zytunes/config.toml,\n\
                    or pass a directory: zytunes photo-sync <dir>")?;
            cmd_photo_sync(&dir)
        }
        "video-sync" => {
            let dir = args
                .get(2)
                .map(|s| s.to_string())
                .or_else(|| std::env::var("ZYTUNES_VIDEOS_DIR").ok())
                .or_else(|| load_config_field("video_dir"))
                .ok_or("No video directory configured.\n\
                    Set ZYTUNES_VIDEOS_DIR env var, add video_dir to ~/.config/zytunes/config.toml,\n\
                    or pass a directory: zytunes video-sync <dir>")?;
            cmd_video_sync(&dir)
        }
        "help" | "--help" | "-h" => {
            println!("zytunes v0.3.0 — sync music to a Zune 30\n");
            println!("Usage: zytunes <command> [args...]\n");
            println!("Commands:");
            println!("  ls [path]              List device contents (default: /)");
            println!("  push <files...>        Push music files to the Zune");
            println!("  rm <device-paths...>   Remove files/folders from the Zune");
            println!("  sync <type> <name>     Sync music to the Zune");
            println!("  photo-sync [dir]       Sync photos to the Zune");
            println!("  video-sync [dir]       Sync videos to the Zune");
            println!("  library [query]        Browse the music library");
            println!("  help                   Show this help");
            println!("\nSync types:");
            println!("  sync artist <name>     Sync all tracks by an artist");
            println!("  sync album <name>      Sync all tracks in an album");
            println!("  sync track <name>      Sync a single track by name");
            println!("\nUnsupported formats (FLAC, OGG, WAV, M4A, OPUS, etc.)");
            println!("are auto-transcoded to MP3 with album art.");
            println!("\nPhotos are resized to fit the Zune 30 screen (240x320).");
            println!("\nSet ZYTUNES_MUSIC_DIR or music_dir in ~/.config/zytunes/config.toml");
            println!("to point at your music folder.");
            println!("\nExamples:");
            println!("  zytunes sync artist \"Radiohead\"");
            println!("  zytunes sync album \"OK Computer\"");
            println!("  zytunes push song.mp3");
            println!("  zytunes photo-sync ~/Pictures/zune-wallpapers");
            println!("  zytunes video-sync ~/Videos/zune");
            println!("  zytunes ls /Music");
            println!("  zytunes rm \"/Music/Artist/Album\"");
            Ok(())
        }
        other => Err(format!(
            "Unknown command: {other}\nRun 'zytunes help' for usage."
        )),
    }
}

/// Browse the music library.
fn cmd_library(query: Option<&str>) -> Result<(), String> {
    println!("Loading music library...");
    let start = std::time::Instant::now();
    let lib = zytunes::load_library(load_config_field("music_dir").as_deref())?;
    println!(
        "Loaded {} tracks in {:.1}s\n",
        lib.track_count(),
        start.elapsed().as_secs_f64()
    );

    if let Some(music_folder) = lib.music_folder() {
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
        Some("stats") | None => {
            let artists = lib.artists();
            let albums = lib.albums();
            println!("\nStats:");
            println!("  {} tracks", lib.track_count());
            println!("  {} artists", artists.len());
            println!("  {} albums", albums.len());

            // Format breakdown.
            let mut formats: HashMap<String, usize> = HashMap::new();
            for t in lib.all_tracks() {
                let kind = t.kind.as_deref().unwrap_or("Unknown");
                *formats.entry(kind.to_string()).or_default() += 1;
            }
            println!("\nFormats:");
            let mut fmts: Vec<_> = formats.iter().collect();
            fmts.sort_by(|a, b| b.1.cmp(a.1));
            for (kind, count) in fmts {
                println!("  {count:>6}  {kind}");
            }
        }
        Some(q) => {
            let tracks = lib.artist_tracks(q);
            if tracks.is_empty() {
                println!("No artist matching \"{q}\"");
            } else {
                println!("Artist \"{q}\": {} tracks", tracks.len());
                for t in &tracks {
                    println!(
                        "  {} - {} ({})",
                        t.album,
                        t.name,
                        t.kind.as_deref().unwrap_or("?")
                    );
                }
            }
        }
    }
    Ok(())
}

/// Sync tracks from the music library to the Zune.
fn cmd_sync(args: &[String]) -> Result<(), String> {
    if args.len() < 2 {
        return Err("Usage: zytunes sync <type> <name>".into());
    }

    let sync_type: zytunes::SyncType = args[0].parse()?;
    let name = &args[1];

    println!("Loading music library...");
    let start = std::time::Instant::now();
    let lib = zytunes::load_library(load_config_field("music_dir").as_deref())?;
    println!(
        "Loaded {} tracks in {:.1}s\n",
        lib.track_count(),
        start.elapsed().as_secs_f64()
    );

    // Find matching tracks.
    let tracks = find_matching_tracks(lib.as_ref(), sync_type, name)?;

    // Filter to tracks that have a file location.
    let pushable: Vec<&zytunes::library::Track> = tracks
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

    // Connect to device.
    let (mut session, caps, _detected) = connect()?;
    println!();

    // Check which need transcoding.
    let to_transcode = pushable
        .iter()
        .filter(|t| {
            t.location
                .as_ref()
                .is_some_and(|loc| needs_transcoding(loc, caps.supported_formats))
        })
        .count();
    println!(
        "\n{} tracks to push ({} need transcoding)",
        pushable.len(),
        to_transcode
    );

    // Create temp dir for transcoded files.
    let temp_dir = make_transcode_temp_dir();
    if to_transcode > 0 {
        std::fs::create_dir_all(&temp_dir)
            .map_err(|e| format!("Failed to create temp directory: {e}"))?;
    }

    let result = sync_to_device(session.as_mut(), &pushable, &temp_dir, &caps)?;

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
    let (mut session, _caps, _detected) = connect()?;
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

    // Connect to device.
    let (mut session, caps, _detected) = connect()?;
    println!();

    // Check which files need transcoding.
    let needs_transcode = files
        .iter()
        .filter(|f| needs_transcoding(f, caps.supported_formats))
        .count();

    println!("Found {} music file(s) to push", files.len());
    if needs_transcode > 0 {
        println!(
            "  {} file(s) will be transcoded to MP3 (device doesn't support FLAC/OGG/etc.)",
            needs_transcode
        );
    }
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

        match transcode_and_import(session.as_mut(), file, &temp_dir, &caps) {
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

/// Sync photos from a directory to the Zune.
/// Photos are resized to fit within 240x320 and encoded as JPEG.
fn cmd_photo_sync(dir: &str) -> Result<(), String> {
    let files = collect_photo_files(&[dir]);
    if files.is_empty() {
        return Err(format!("No photo files found in {}", dir));
    }

    println!("Found {} photo(s) to sync", files.len());

    let (mut session, _caps, _detected) = connect()?;
    println!();

    // Get existing photos on device to skip duplicates.
    let existing: std::collections::HashSet<String> = session
        .ls("/Photos")
        .unwrap_or_default()
        .iter()
        .map(|e| e.name.clone())
        .collect();

    let mut success = 0;
    let mut skipped = 0;
    let mut failed = 0;
    let total = files.len();

    for (i, file) in files.iter().enumerate() {
        let filename = Path::new(file)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();

        // Convert filename to JPEG extension for the device.
        let device_filename = format!(
            "{}.jpg",
            Path::new(&*filename)
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
        );

        if existing.contains(&device_filename) {
            println!(
                "[{}/{}] {} ... skipped (already on device)",
                i + 1,
                total,
                filename
            );
            skipped += 1;
            continue;
        }

        print!("[{}/{}] {} ... ", i + 1, total, filename);
        match resize_photo_for_zune(file) {
            Ok(jpeg_data) => match session.import_photo(&device_filename, &jpeg_data) {
                Ok(_id) => {
                    println!("OK");
                    success += 1;
                }
                Err(e) => {
                    println!("FAILED: {}", e);
                    failed += 1;
                }
            },
            Err(e) => {
                println!("FAILED (resize): {}", e);
                failed += 1;
            }
        }
    }

    println!(
        "\nDone: {} synced, {} skipped, {} failed",
        success, skipped, failed
    );
    Ok(())
}

/// Sync videos from a directory to the Zune.
fn cmd_video_sync(dir: &str) -> Result<(), String> {
    let files = collect_video_files(&[dir]);
    if files.is_empty() {
        return Err(format!("No video files found in {}", dir));
    }

    // Check if any files need transcoding and verify ffmpeg is available.
    let needs_transcode: Vec<_> = files
        .iter()
        .filter(|f| needs_video_transcoding(f))
        .collect();
    if !needs_transcode.is_empty() && !check_ffmpeg_available() {
        return Err(format!(
            "{} video(s) need transcoding to WMV but ffmpeg is not installed.\n\
             Install ffmpeg or convert files to WMV manually.",
            needs_transcode.len()
        ));
    }

    println!("Found {} video(s) to sync", files.len());
    if !needs_transcode.is_empty() {
        println!(
            "  ({} will be transcoded to WMV via ffmpeg)",
            needs_transcode.len()
        );
    }

    let (mut session, _caps, _detected) = connect()?;
    println!();

    let temp_dir = make_transcode_temp_dir();

    // Get existing videos on device to skip duplicates.
    // Check both original filename and .wmv variant for transcoded files.
    let existing: std::collections::HashSet<String> = session
        .ls("/Videos")
        .unwrap_or_default()
        .iter()
        .map(|e| e.name.clone())
        .collect();

    let mut success = 0;
    let mut skipped = 0;
    let mut failed = 0;
    let total = files.len();

    for (i, file) in files.iter().enumerate() {
        let filename = Path::new(file)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        // For non-WMV files, the device filename will be stem.wmv after transcoding.
        let device_filename = if needs_video_transcoding(file) {
            let stem = Path::new(file)
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy();
            format!("{stem}.wmv")
        } else {
            filename.clone()
        };

        if existing.contains(&device_filename) {
            println!(
                "[{}/{}] {} ... skipped (already on device)",
                i + 1,
                total,
                filename
            );
            skipped += 1;
            continue;
        }

        if needs_video_transcoding(file) {
            print!("[{}/{}] {} (transcoding) ... ", i + 1, total, filename);
        } else {
            print!("[{}/{}] {} ... ", i + 1, total, filename);
        }

        match transcode_and_import_video(session.as_mut(), file, &temp_dir) {
            Ok(_id) => {
                println!("OK");
                success += 1;
            }
            Err(e) => {
                println!("FAILED: {}", e);
                failed += 1;
            }
        }
    }

    // Clean up temp directory.
    let _ = std::fs::remove_dir_all(&temp_dir);

    println!(
        "\nDone: {} synced, {} skipped, {} failed",
        success, skipped, failed
    );
    Ok(())
}

/// Remove files/folders from the device.
fn cmd_rm(paths: &[String]) -> Result<(), String> {
    let (mut session, _caps, _detected) = connect()?;
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
