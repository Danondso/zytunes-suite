use zytunes::{
    check_ffmpeg_available, collect_music_files, collect_photo_files, collect_video_files, connect,
    find_matching_tracks, make_transcode_temp_dir, needs_transcoding, needs_video_transcoding,
    resize_photo_for_zune, sync_to_device, transcode_and_import, transcode_and_import_video,
};

use std::collections::HashMap;
use std::path::Path;

fn main() {
    // Suppress panic prints from rayon worker threads. The library scan calls
    // symphonia + rusty-chromaprint per file in parallel, both of which have
    // known panic paths on edge-case audio (see `compute_fingerprint`).
    // `catch_unwind` already absorbs the panic and turns it into `None`, but
    // without this hook the default handler still prints the three-line
    // `thread '<unnamed>' panicked at ...` block per panicking file —
    // hundreds of lines for a modest library, all noise. Main-thread panics
    // still print so genuine bugs surface.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if std::thread::current().name() == Some("main") {
            default_hook(info);
        }
    }));

    let args: Vec<String> = std::env::args().collect();
    if let Err(e) = run(&args) {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

/// Load a string field from the TOML config file at
/// ~/.config/zytunes/config.toml.
fn load_config_field(field: &str) -> Option<String> {
    load_config_value(field).and_then(|v| v.as_str().map(|s| s.to_string()))
}

/// Load a boolean field from the same config. Returns `None` for absent or
/// non-bool values; callers apply their own default.
fn load_config_bool(field: &str) -> Option<bool> {
    load_config_value(field).and_then(|v| v.as_bool())
}

fn load_config_value(field: &str) -> Option<toml::Value> {
    let home = std::env::var("HOME").ok()?;
    let path = Path::new(&home)
        .join(".config")
        .join("zytunes")
        .join("config.toml");
    let contents = std::fs::read_to_string(path).ok()?;
    let table: toml::Table = contents.parse().ok()?;
    table.get(field).cloned()
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
        "probe" => cmd_probe(&args[2..]),
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
            println!("\nDiagnostics:");
            println!("  probe props [format]   List MTP properties the Zune advertises");
            println!(
                "                         (format: 0x3009 MP3 [default], 0xB901 WMA, 0xB903 AAC)"
            );
            println!("  probe playcount [obj]  Read playcount-related props for an audio object");
            println!("                         (uses first audio object if obj-id omitted)");
            println!("  probe zmdb-dump <out>  Dump raw ZMDB binary for diff-based analysis");
            println!("                         (firmware 3.0+ only)");
            println!("  probe all-props [obj]  Ask the device for ALL props (bypasses the");
            println!("                         supported-list — best v1.4 path)");
            println!("  probe vendor-op <code> [params...]  Invoke a raw vendor op for survey");
            println!("                         (read-only ops only; allowlist excludes 0x9180)");
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
    let opts = zytunes::dirlib::ScanOptions {
        fingerprint: load_config_bool("fingerprinting").unwrap_or(true),
    };
    let lib = zytunes::load_library_with_options(load_config_field("music_dir").as_deref(), opts)?;
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

            // Acoustic-fingerprint coverage. Useful for diagnosing whether
            // the cache is being persisted between scans — if this stays
            // low after a successful run, the cache write probably failed
            // (look for "zytunes: cache: write … failed" on stderr).
            let total = lib.track_count();
            let with_fp = lib.all_tracks().filter(|t| t.acoustic_id.is_some()).count();
            let pct = if total == 0 {
                0.0
            } else {
                100.0 * with_fp as f64 / total as f64
            };
            println!("\nFingerprints: {with_fp}/{total} ({pct:.1}%)");

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
            let tracks: Vec<&zytunes::library::Track> = lib.artist_tracks(q).collect();
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

        match transcode_and_import(session.as_mut(), file, &temp_dir, &caps, None) {
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

/// Diagnostic probe subcommand used to investigate what playcount-related
/// MTP properties the Zune exposes, and to snapshot the ZMDB binary for
/// before/after diff. Zune-only — connects directly via the ZuneBackend
/// instead of the generic `connect()` so it can access the concrete
/// `NativeSession` and its probe methods.
fn cmd_probe(args: &[String]) -> Result<(), String> {
    let sub = args
        .first()
        .map(|s| s.as_str())
        .ok_or("Usage: zytunes probe <props|playcount|zmdb-dump|all-props|vendor-op> [args...]")?;

    // Detect + open a Zune session. We bypass the generic connect() because
    // it returns Box<dyn DeviceSession>; the probe methods live on the
    // concrete NativeSession and this is a Zune-only command anyway.
    use zytunes::device::zune::{ZuneBackend, ZuneDeviceData};
    use zytunes::device::DeviceBackend;
    let backend = ZuneBackend;
    let detected = backend.detect()?;
    println!("Detected: {}", detected.name);
    let data = detected
        .backend_data
        .downcast_ref::<ZuneDeviceData>()
        .ok_or("Internal: Zune detection returned wrong backend data")?;
    let mut session = zytunes::mtp::NativeSession::open(data.product_id, &|msg: &str| {
        eprintln!("{msg}");
    })?;
    println!("Session ready.\n");

    match sub {
        "props" => {
            let fmt = args
                .get(1)
                .map(|s| parse_u16_maybe_hex(s))
                .transpose()?
                .unwrap_or(0x3009);
            print_known_format_label(fmt);
            let props = session.probe_supported_props(fmt)?;
            if props.is_empty() {
                println!("Device advertised no properties for format 0x{fmt:04X}.");
                println!("(This is unexpected for a supported audio format — file with the session state.)");
            } else {
                println!("Advertised properties ({}):", props.len());
                for p in &props {
                    let label = prop_label(*p);
                    println!("  0x{p:04X}  {label}");
                }
            }
            Ok(())
        }
        "playcount" => {
            let target = match args.get(1) {
                Some(s) => parse_u32_maybe_hex(s)?,
                None => match session.probe_first_audio_handle()? {
                    Some((h, fmt)) => {
                        println!("Auto-selected first audio object: handle=0x{h:08X} format=0x{fmt:04X}");
                        h
                    }
                    None => {
                        return Err(
                            "No audio objects found on device — sync a track first.".into()
                        )
                    }
                },
            };
            println!();
            probe_playcount_props(&mut session, target)
        }
        "zmdb-dump" => {
            let out = args
                .get(1)
                .ok_or("Usage: zytunes probe zmdb-dump <output-path>")?;
            let bytes = session.probe_zmdb_dump()?;
            std::fs::write(out, &bytes).map_err(|e| format!("write {out}: {e}"))?;
            println!("Wrote {} bytes to {out}", bytes.len());
            println!("\nNext steps:");
            println!(
                "  1. Play a track on the Zune past the 50%-or-4-minute mark (iTunes threshold)."
            );
            println!("  2. Re-run `zytunes probe zmdb-dump` to a different output path.");
            println!("  3. Diff the two dumps with e.g. `cmp -l before.bin after.bin | head`.");
            println!("     Bytes that changed identify where the playcount lives.");
            Ok(())
        }
        "all-props" => {
            let target = match args.get(1) {
                Some(s) => parse_u32_maybe_hex(s)?,
                None => match session.probe_first_audio_handle()? {
                    Some((h, fmt)) => {
                        println!(
                            "Auto-selected first audio object: handle=0x{h:08X} format=0x{fmt:04X}"
                        );
                        h
                    }
                    None => {
                        return Err(
                            "No audio objects found on device — sync a track first.".into()
                        )
                    }
                },
            };
            println!();
            probe_all_props(&mut session, target)
        }
        "vendor-op" => {
            let code_str = args
                .get(1)
                .ok_or("Usage: zytunes probe vendor-op <hex-code> [param1 param2 ...]")?;
            let code = parse_u16_maybe_hex(code_str)?;
            let params: Result<Vec<u32>, String> =
                args.iter().skip(2).map(|s| parse_u32_maybe_hex(s)).collect();
            let params = params?;
            println!(
                "Calling op 0x{code:04X} with {} param(s): {:?}",
                params.len(),
                params
            );
            match session.probe_vendor_op(code, &params) {
                Ok(bytes) => {
                    println!(
                        "OK — {} byte(s) returned: {}",
                        bytes.len(),
                        preview_bytes(&bytes)
                    );
                    if bytes.len() > 16 {
                        // Dump first 256 bytes hex+ascii for inspection.
                        let n = bytes.len().min(256);
                        println!("\nFirst {n} bytes:");
                        for chunk in bytes[..n].chunks(16) {
                            let hex: String = chunk
                                .iter()
                                .map(|b| format!("{b:02x} "))
                                .collect();
                            let ascii: String = chunk
                                .iter()
                                .map(|&b| {
                                    if (0x20..=0x7E).contains(&b) {
                                        b as char
                                    } else {
                                        '.'
                                    }
                                })
                                .collect();
                            println!("  {hex:<48} {ascii}");
                        }
                    }
                    Ok(())
                }
                Err(e) => Err(format!("op 0x{code:04X} failed: {e}")),
            }
        }
        other => Err(format!(
            "Unknown probe subcommand: {other}\nUsage: zytunes probe <props|playcount|zmdb-dump|all-props|vendor-op> [args...]"
        )),
    }
}

/// Bypass `GetObjectPropsSupported` and ask the device directly for every
/// property it has on `object_id`. Devices commonly serve unadvertised
/// properties — particularly Microsoft-specific MTP-AAS extensions on the
/// Zune — so this is the highest-yield probe on v1.4 hardware where the
/// supported-list is conservative.
fn probe_all_props(
    session: &mut zytunes::mtp::NativeSession,
    object_id: u32,
) -> Result<(), String> {
    use zune_mtp::proplist::{
        PROP_DATE_ADDED, PROP_LAST_ACCESSED, PROP_RATING, PROP_SKIP_COUNT, PROP_USE_COUNT,
    };
    let elements = session.probe_all_props(object_id)?;
    if elements.is_empty() {
        println!("Device returned no properties for handle 0x{object_id:08X}.");
        println!("(Either the object id is wrong or this op isn't supported.)");
        return Ok(());
    }
    println!(
        "GetObjectPropList returned {} properties for object 0x{object_id:08X}:",
        elements.len()
    );
    println!();
    println!("  {:<6}  {:<32}  {:<8}  VALUE", "PROP", "LABEL", "TYPE");
    for e in &elements {
        let label = prop_label(e.prop_code);
        let type_name = mtp_datatype_name(e.datatype);
        let mut value_str = preview_bytes(&e.value);
        // For STR-typed values, render the decoded UCS-2LE string instead
        // of the raw byte preview — much more useful for diagnostics.
        if let Some(s) = e.as_string() {
            value_str = format!("\"{s}\"");
        }
        println!(
            "  0x{:04X}  {:<32}  {:<8}  {}",
            e.prop_code, label, type_name, value_str
        );
    }
    println!();

    // Highlight playcount-related findings explicitly so the user doesn't
    // have to scan the whole list.
    let playcount_props = [
        (PROP_USE_COUNT, "UseCount"),
        (PROP_SKIP_COUNT, "SkipCount"),
        (PROP_LAST_ACCESSED, "LastAccessed"),
        (PROP_RATING, "Rating"),
        (PROP_DATE_ADDED, "DateAdded"),
    ];
    let found: Vec<_> = playcount_props
        .iter()
        .filter_map(|(code, label)| {
            elements
                .iter()
                .find(|e| e.prop_code == *code)
                .map(|e| (*code, *label, e))
        })
        .collect();
    if found.is_empty() {
        println!("No playcount-adjacent props in the response.");
        println!("If GetObjectPropsSupported also didn't list them, this Zune doesn't expose");
        println!("playcount via standard MTP-AAS. Try `probe vendor-op` to survey the");
        println!("Microsoft 0x91xx range (carefully — see findings.md for known-bad codes).");
    } else {
        println!("Playcount-adjacent props found:");
        for (code, label, e) in found {
            let value = e
                .as_u32()
                .map(|v| format!("u32={v}"))
                .or_else(|| e.as_u16().map(|v| format!("u16={v}")))
                .unwrap_or_else(|| preview_bytes(&e.value));
            println!("  0x{code:04X}  {label}  →  {value}");
        }
    }
    Ok(())
}

fn mtp_datatype_name(t: u16) -> &'static str {
    match t {
        0x0000 => "UNDEF",
        0x0001 => "INT8",
        0x0002 => "UINT8",
        0x0003 => "INT16",
        0x0004 => "UINT16",
        0x0005 => "INT32",
        0x0006 => "UINT32",
        0x0007 => "INT64",
        0x0008 => "UINT64",
        0x0009 => "INT128",
        0x000A => "UINT128",
        0x4002 => "AUINT8",
        0x4004 => "AUINT16",
        0x4006 => "AUINT32",
        0x4008 => "AUINT64",
        0xFFFF => "STR",
        _ => "?",
    }
}

/// Query playcount-adjacent properties on `object_id` and report what the
/// device returns. Uses `GetObjectPropList(handle, 0, 0xFFFFFFFF, 0, 0)` —
/// the bulk path — since v1.4 firmware rejects the per-property
/// `GetObjectPropValue` op for many of these codes (`0xa801
/// InvalidObjectPropCode`) even when the prop *is* served via the bulk op.
/// The bulk path is the truth.
fn probe_playcount_props(
    session: &mut zytunes::mtp::NativeSession,
    object_id: u32,
) -> Result<(), String> {
    use zune_mtp::proplist::{
        PROP_DATE_ADDED, PROP_LAST_ACCESSED, PROP_RATING, PROP_SKIP_COUNT, PROP_USE_COUNT,
    };

    let targets = [
        (PROP_USE_COUNT, "UseCount (playcount)"),
        (PROP_SKIP_COUNT, "SkipCount"),
        (PROP_LAST_ACCESSED, "LastAccessed"),
        (PROP_RATING, "Rating"),
        (PROP_DATE_ADDED, "DateAdded"),
    ];

    let elements = session.probe_all_props(object_id)?;

    println!("Querying playcount-adjacent properties on object 0x{object_id:08X}:");
    println!("(via GetObjectPropList; absent props are not exposed by firmware)");
    println!();

    let mut any_present = false;
    for (code, label) in targets {
        match elements.iter().find(|e| e.prop_code == code) {
            Some(e) => {
                any_present = true;
                let value = e
                    .as_u32()
                    .map(|v| format!("u32={v}"))
                    .or_else(|| e.as_u16().map(|v| format!("u16={v}")))
                    .unwrap_or_else(|| preview_bytes(&e.value));
                println!("  0x{code:04X}  {label:<22}  exposed   {value}");
            }
            None => {
                println!("  0x{code:04X}  {label:<22}  not exposed by firmware");
            }
        }
    }
    println!();
    println!("Interpretation:");
    if any_present {
        println!("  - 'exposed' props can be populated via `session.get_object_prop_list(...)`.");
        println!("  - 'not exposed' props are absent from the device's response and cannot be");
        println!("    read on this firmware. Zune v1.4 does not surface SkipCount, LastAccessed,");
        println!("    or DateAdded on either MTP path — confirmed via the full-prop dump");
        println!("    (`zytunes probe all-props {object_id:#X}`).");
    } else {
        println!("  - The device returned no playcount-adjacent props at all. Use");
        println!("    `zytunes probe all-props {object_id:#X}` to inspect what it does serve.");
    }
    Ok(())
}

fn prop_label(code: u16) -> &'static str {
    match code {
        0xDC01 => "StorageID",
        0xDC02 => "ObjectFormat",
        0xDC03 => "ProtectionStatus",
        0xDC04 => "ObjectSize",
        0xDC07 => "ObjectFileName",
        0xDC08 => "DateCreated",
        0xDC09 => "DateModified",
        0xDC0B => "ParentObject",
        0xDC41 => "PersistentUniqueObjectIdentifier",
        0xDC44 => "Name",
        0xDC46 => "Artist",
        0xDC47 => "DateAuthored",
        0xDC4E => "DateAdded",
        0xDC86 => "RepresentativeSampleData",
        0xDC89 => "Duration",
        0xDC8A => "Rating",
        0xDC8B => "Track",
        0xDC8C => "Genre",
        0xDC8E => "Lyrics",
        0xDC91 => "UseCount (playcount)",
        0xDC92 => "SkipCount",
        0xDC93 => "LastAccessed",
        0xDC95 => "MetaGenre",
        0xDAB9 => "ArtistId (Zune-specific)",
        _ => "(unknown — look up in MTP spec / libmtp)",
    }
}

fn print_known_format_label(fmt: u16) {
    let label = match fmt {
        0x3009 => "MP3",
        0x3008 => "WAV",
        0xB901 => "WMA",
        0xB903 => "AAC / MP4 audio",
        0xB982 => "MP4 container",
        0xB984 => "FLAC",
        _ => "unknown format",
    };
    println!("Querying supported properties for format 0x{fmt:04X} ({label})\n");
}

fn parse_u16_maybe_hex(s: &str) -> Result<u16, String> {
    let trimmed = s.trim_start_matches("0x").trim_start_matches("0X");
    if trimmed != s {
        u16::from_str_radix(trimmed, 16).map_err(|e| format!("bad hex u16 {s:?}: {e}"))
    } else {
        s.parse::<u16>().map_err(|e| format!("bad u16 {s:?}: {e}"))
    }
}

fn parse_u32_maybe_hex(s: &str) -> Result<u32, String> {
    let trimmed = s.trim_start_matches("0x").trim_start_matches("0X");
    if trimmed != s {
        u32::from_str_radix(trimmed, 16).map_err(|e| format!("bad hex u32 {s:?}: {e}"))
    } else {
        s.parse::<u32>().map_err(|e| format!("bad u32 {s:?}: {e}"))
    }
}

/// Format raw prop bytes for display. Renders as a little-endian u32 when
/// the length is 4 (most integer props); otherwise hex-dump the first 16
/// bytes so strings and blobs are still legible.
fn preview_bytes(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return "(empty)".into();
    }
    if bytes.len() == 4 {
        let v = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        return format!("u32={v}");
    }
    if bytes.len() == 2 {
        let v = u16::from_le_bytes([bytes[0], bytes[1]]);
        return format!("u16={v}");
    }
    let head: Vec<String> = bytes.iter().take(16).map(|b| format!("{b:02x}")).collect();
    let tail = if bytes.len() > 16 { "…" } else { "" };
    format!("hex=[{}{tail}]", head.join(" "))
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

    #[test]
    fn parse_u16_maybe_hex_accepts_hex_and_decimal() {
        assert_eq!(parse_u16_maybe_hex("0x3009").unwrap(), 0x3009);
        assert_eq!(parse_u16_maybe_hex("0X3009").unwrap(), 0x3009);
        assert_eq!(parse_u16_maybe_hex("12297").unwrap(), 12_297);
        assert!(parse_u16_maybe_hex("0xZZZZ").is_err());
        assert!(parse_u16_maybe_hex("not-a-number").is_err());
    }

    #[test]
    fn parse_u32_maybe_hex_accepts_hex_and_decimal() {
        assert_eq!(parse_u32_maybe_hex("0xDEADBEEF").unwrap(), 0xDEAD_BEEF);
        assert_eq!(parse_u32_maybe_hex("42").unwrap(), 42);
        assert!(parse_u32_maybe_hex("0xG").is_err());
    }

    #[test]
    fn preview_bytes_decodes_u32_le() {
        assert_eq!(preview_bytes(&[0x2A, 0x00, 0x00, 0x00]), "u32=42");
        assert_eq!(preview_bytes(&[0xFF, 0xFF, 0xFF, 0xFF]), "u32=4294967295");
    }

    #[test]
    fn preview_bytes_decodes_u16_le() {
        assert_eq!(preview_bytes(&[0x2A, 0x00]), "u16=42");
    }

    #[test]
    fn preview_bytes_falls_back_to_hex() {
        assert_eq!(preview_bytes(&[]), "(empty)");
        let long = vec![0xABu8; 32];
        let got = preview_bytes(&long);
        assert!(got.starts_with("hex=[ab ab ab"));
        assert!(got.ends_with("…]"));
    }

    #[test]
    fn prop_label_identifies_playcount_family() {
        assert_eq!(prop_label(0xDC91), "UseCount (playcount)");
        assert_eq!(prop_label(0xDC92), "SkipCount");
        assert_eq!(prop_label(0xDC93), "LastAccessed");
        assert!(prop_label(0xFFFF).starts_with("(unknown"));
    }
}
