use zytunes::library::ItunesLibrary;
use zytunes::mtp::DeviceSession;
use zytunes::{
    collect_music_files, connect, find_matching_tracks, library_xml_path,
    make_transcode_temp_dir, needs_transcoding, sync_to_device, transcode_and_import,
};

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

fn main() {
    let args: Vec<String> = std::env::args().collect();
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
            let default = library_xml_path();
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
            println!("zytunes v0.3.0 — sync music to a Zune 30\n");
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

/// Sync tracks from iTunes library to Zune.
fn cmd_sync(args: &[String]) -> Result<(), String> {
    if args.len() < 2 {
        return Err("Usage: zytunes sync <type> <name> [--library <path>]".into());
    }

    let sync_type = args[0].as_str();
    let name = &args[1];

    // Check for --library flag.
    let default = library_xml_path();
    let xml_path = args
        .iter()
        .position(|a| a == "--library")
        .and_then(|i| args.get(i + 1))
        .map(|s| s.as_str())
        .unwrap_or(&default);

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
