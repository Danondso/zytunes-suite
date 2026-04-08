//! Validation tool: reads a real iTunesDB from a connected iPod and exercises the parser.
//!
//! Usage:
//!   cargo run -p ipod-db --example validate_ipod
//!   cargo run -p ipod-db --example validate_ipod -- /path/to/ipod/mount
//!
//! What it does:
//!   1. Detects the iPod (or uses the provided mount path)
//!   2. Reads iPod_Control/Device/SysInfo and prints device info
//!   3. Reads the raw iTunesDB binary and attempts to parse it
//!   4. Prints summary stats: track count, playlist count, sample tracks
//!   5. Cross-references parsed tracks against actual files in F-dirs
//!   6. Attempts serialize → re-parse round-trip and compares results

use ipod_db::{detect, fs, itunesdb, itunesdb_write};
use std::path::PathBuf;

fn main() {
    let mount = match std::env::args().nth(1) {
        Some(path) => {
            let p = PathBuf::from(&path);
            if !p.join("iPod_Control").is_dir() {
                eprintln!("ERROR: {path} does not contain iPod_Control/");
                std::process::exit(1);
            }
            println!("Using provided mount path: {path}");
            p
        }
        None => {
            println!("Scanning for connected iPods...");
            match detect::detect_ipod() {
                Ok(ipod) => {
                    println!("Found iPod at: {}", ipod.mount_point.display());
                    if let Some(ref m) = ipod.model {
                        println!("  Model:    {m}");
                    }
                    if let Some(ref s) = ipod.serial {
                        println!("  Serial:   {s}");
                    }
                    if let Some(ref f) = ipod.firmware_version {
                        println!("  Firmware: {f}");
                    }
                    ipod.mount_point
                }
                Err(e) => {
                    eprintln!("ERROR: {e}");
                    eprintln!("Tip: pass the mount path as an argument, or set IPOD_MOUNT_PATH");
                    std::process::exit(1);
                }
            }
        }
    };

    // --- Step 1: Read raw iTunesDB ---
    let db_path = mount.join("iPod_Control").join("iTunes").join("iTunesDB");
    if !db_path.exists() {
        eprintln!("ERROR: iTunesDB not found at {}", db_path.display());
        eprintln!("This iPod may not have been initialized by iTunes yet.");
        std::process::exit(1);
    }

    let raw = match std::fs::read(&db_path) {
        Ok(data) => data,
        Err(e) => {
            eprintln!("ERROR: failed to read iTunesDB: {e}");
            std::process::exit(1);
        }
    };
    println!("\n=== iTunesDB ===");
    println!("Path: {}", db_path.display());
    println!(
        "Size: {} bytes ({:.1} KB)",
        raw.len(),
        raw.len() as f64 / 1024.0
    );

    // --- Step 2: Parse ---
    println!("\n=== Parsing ===");
    let db = match itunesdb::parse(&raw, mount.clone()) {
        Ok(db) => {
            println!("SUCCESS: parsed iTunesDB v{:#x}", db.db_version);
            db
        }
        Err(e) => {
            eprintln!("PARSE FAILED: {e}");
            eprintln!("\nThis likely means a byte offset is wrong in our parser.");
            eprintln!("Raw hex dump of first 128 bytes:");
            hex_dump(&raw[..raw.len().min(128)]);

            // Try to show where it failed by dumping around the error.
            std::process::exit(1);
        }
    };

    // --- Step 3: Summary ---
    println!("\n=== Database Summary ===");
    println!("Tracks:    {}", db.tracks.len());
    println!("Playlists: {}", db.playlists.len());

    if !db.tracks.is_empty() {
        println!("\n--- Sample Tracks (first 10) ---");
        for (i, t) in db.tracks.iter().take(10).enumerate() {
            println!(
                "  {i:>2}. [dbid={}, tid={}] \"{}\" by \"{}\" on \"{}\"",
                t.dbid, t.track_id, t.title, t.artist, t.album
            );
            println!(
                "      path={} size={} type={:#010x}",
                t.ipod_path, t.file_size, t.filetype
            );
            if let Some(ms) = t.total_time_ms {
                let secs = ms / 1000;
                println!("      duration={}:{:02}", secs / 60, secs % 60);
            }
        }
    }

    for pl in &db.playlists {
        let kind = if pl.is_master { " (master)" } else { "" };
        println!(
            "  Playlist: \"{}\"{} — {} tracks",
            pl.name,
            kind,
            pl.track_ids.len()
        );
    }

    // --- Step 4: Cross-reference with filesystem ---
    println!("\n=== Filesystem Cross-Reference ===");
    match fs::enumerate_audio_files(&mount) {
        Ok(files) => {
            println!("Audio files on disk: {}", files.len());
            println!("Tracks in database: {}", db.tracks.len());
            if files.len() == db.tracks.len() {
                println!("MATCH: counts are equal");
            } else {
                println!(
                    "MISMATCH: {} file(s) difference",
                    (files.len() as i64 - db.tracks.len() as i64).abs()
                );
            }

            // Check that each track's ipod_path points to a real file.
            let mut missing = 0;
            for t in &db.tracks {
                let real = fs::real_path(&mount, &t.ipod_path);
                if !real.exists() {
                    if missing < 5 {
                        println!("  MISSING: {} -> {}", t.ipod_path, real.display());
                    }
                    missing += 1;
                }
            }
            if missing > 5 {
                println!("  ... and {} more missing files", missing - 5);
            }
            if missing == 0 {
                println!("All track paths resolve to existing files");
            } else {
                println!("WARN: {missing} track(s) point to missing files");
            }
        }
        Err(e) => {
            println!("Could not enumerate files: {e}");
        }
    }

    // --- Step 5: Round-trip test ---
    println!("\n=== Round-Trip Test ===");
    let serialized = itunesdb_write::serialize(&db);
    println!("Serialized size: {} bytes", serialized.len());
    println!("Original size:   {} bytes", raw.len());

    match itunesdb::parse(&serialized, mount.clone()) {
        Ok(db2) => {
            println!("Re-parse: SUCCESS");
            println!(
                "Tracks: {} -> {} ({})",
                db.tracks.len(),
                db2.tracks.len(),
                if db.tracks.len() == db2.tracks.len() {
                    "match"
                } else {
                    "MISMATCH"
                }
            );
            println!(
                "Playlists: {} -> {} ({})",
                db.playlists.len(),
                db2.playlists.len(),
                if db.playlists.len() == db2.playlists.len() {
                    "match"
                } else {
                    "MISMATCH"
                }
            );

            // Spot-check first track.
            if let (Some(a), Some(b)) = (db.tracks.first(), db2.tracks.first()) {
                let fields_match = a.title == b.title
                    && a.artist == b.artist
                    && a.album == b.album
                    && a.ipod_path == b.ipod_path
                    && a.file_size == b.file_size
                    && a.dbid == b.dbid;
                println!(
                    "First track fields: {}",
                    if fields_match { "match" } else { "MISMATCH" }
                );
                if !fields_match {
                    println!(
                        "  Original:  \"{}\" by \"{}\" dbid={}",
                        a.title, a.artist, a.dbid
                    );
                    println!(
                        "  Re-parsed: \"{}\" by \"{}\" dbid={}",
                        b.title, b.artist, b.dbid
                    );
                }
            }
        }
        Err(e) => {
            eprintln!("Re-parse FAILED: {e}");
            eprintln!("Our serializer produced something our parser can't read back.");
        }
    }

    // Size difference is expected (we zero out unknown fields, may have different
    // header sizes, skip podcast datasets, etc.)
    if serialized.len() != raw.len() {
        let diff = (serialized.len() as i64 - raw.len() as i64).abs();
        println!(
            "\nNote: size differs by {} bytes — expected since we zero unknown fields \
             and may use different header padding than the original writer.",
            diff
        );
    }

    println!("\n=== Done ===");
}

fn hex_dump(data: &[u8]) {
    for (i, chunk) in data.chunks(16).enumerate() {
        print!("  {:04x}: ", i * 16);
        for b in chunk {
            print!("{b:02x} ");
        }
        // ASCII representation.
        for _ in chunk.len()..16 {
            print!("   ");
        }
        print!(" |");
        for b in chunk {
            if b.is_ascii_graphic() || *b == b' ' {
                print!("{}", *b as char);
            } else {
                print!(".");
            }
        }
        println!("|");
    }
}
