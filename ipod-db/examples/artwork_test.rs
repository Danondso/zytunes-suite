//! Device artwork test: add artwork for existing tracks and write ArtworkDB + .ithmb files.
//!
//! Usage:
//!   cargo run -p ipod-db --example artwork_test -- /mnt/ipod-classic /path/to/cover.jpg [classic|video]
//!
//! This will:
//!   1. Parse the existing iTunesDB
//!   2. Initialize the artwork store for the specified model (default: video)
//!   3. Encode the cover image for every track (all tracks get the same art)
//!   4. Write ArtworkDB + .ithmb files to iPod_Control/Artwork/
//!   5. Rewrite iTunesDB with artwork_count/has_artwork flags set
//!
//! To restore: delete iPod_Control/Artwork/ArtworkDB and F*_1.ithmb files,
//! then copy iTunesDB.bak back to iTunesDB.

use ipod_db::{artwork, hash, itunesdb, itunesdb_write};
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("Usage: artwork_test <mount_path> <cover_image> [classic|video]");
        std::process::exit(1);
    }

    let mount = PathBuf::from(&args[1]);
    let cover_path = PathBuf::from(&args[2]);
    let model = args.get(3).map(|s| s.as_str()).unwrap_or("video");

    let db_path = mount.join("iPod_Control/iTunes/iTunesDB");
    if !db_path.exists() {
        eprintln!("ERROR: no iTunesDB at {}", db_path.display());
        std::process::exit(1);
    }

    // Step 1: Parse existing database.
    println!("Reading iTunesDB...");
    let raw = std::fs::read(&db_path).unwrap();
    let mut db = match itunesdb::parse(&raw, mount.clone()) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("PARSE FAILED: {e}");
            std::process::exit(1);
        }
    };
    println!(
        "  {} tracks, {} playlists",
        db.tracks.len(),
        db.playlists.len()
    );

    if db.tracks.is_empty() {
        eprintln!("No tracks in database — nothing to add artwork to.");
        std::process::exit(1);
    }

    // Step 2: Read cover image.
    println!("Reading cover image: {}", cover_path.display());
    let image_bytes = std::fs::read(&cover_path).unwrap();
    println!("  {} bytes", image_bytes.len());

    // Step 3: Initialize artwork store.
    let specs = match model {
        "classic" => {
            println!("Using iPod Classic specs (128x128 + 320x320)");
            artwork::model_specs_classic()
        }
        "video" => {
            println!("Using iPod Video specs (100x100 + 200x200)");
            artwork::model_specs_video()
        }
        other => {
            eprintln!("Unknown model '{other}', expected 'classic' or 'video'");
            std::process::exit(1);
        }
    };
    db.init_artwork(specs);

    // Step 4: Add artwork for every track.
    println!("Encoding artwork for {} tracks...", db.tracks.len());
    let dbids: Vec<u64> = db.tracks.iter().map(|t| t.dbid).collect();
    for (i, dbid) in dbids.iter().enumerate() {
        if let Err(e) = db.set_track_artwork(*dbid, &image_bytes) {
            eprintln!("  ERROR on track {}: {e}", i + 1);
            std::process::exit(1);
        }
        if (i + 1) % 100 == 0 || i + 1 == dbids.len() {
            println!("  {}/{} tracks", i + 1, dbids.len());
        }
    }

    // Report ithmb file sizes.
    if let Some(ref store) = db.artwork_store {
        for f in &store.ithmb_files {
            println!(
                "  {} — {} bytes ({} images)",
                f.filename,
                f.data.len(),
                f.data.len() / f.image_size as usize
            );
        }
    }

    // Step 5: Confirm.
    println!();
    println!("Ready to write:");
    println!("  - ArtworkDB + .ithmb files to iPod_Control/Artwork/");
    println!("  - Updated iTunesDB with artwork flags");
    println!();
    println!("To restore after writing:");
    println!("  - Copy iTunesDB.bak -> iTunesDB");
    println!("  - Delete iPod_Control/Artwork/ArtworkDB and F*_1.ithmb");
    println!();
    print!("Write? [y/N] ");
    use std::io::Write;
    std::io::stdout().flush().unwrap();

    let mut input = String::new();
    std::io::stdin().read_line(&mut input).unwrap();
    if input.trim().to_lowercase() != "y" {
        println!("Aborted.");
        std::process::exit(0);
    }

    // Step 6: Sign and write.
    let firewire_id = std::env::args().nth(4).or_else(|| {
        let output = std::process::Command::new("lsusb")
            .args(["-v", "-d", "05ac:"])
            .output()
            .ok()?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            if line.contains("iSerial") {
                return line.split_whitespace().last().map(|s| s.to_string());
            }
        }
        None
    });

    let fwid = firewire_id
        .as_deref()
        .and_then(|s| match hash::parse_firewire_id(s) {
            Ok(id) => {
                println!("Signing with hash58 (FirewireGuid: {s})");
                Some(id)
            }
            Err(e) => {
                println!("WARNING: Failed to parse FirewireGuid: {e}");
                None
            }
        });

    println!("Writing...");
    match itunesdb_write::write_to_disk(&db, fwid.as_ref()) {
        Ok(()) => {
            println!("Done!");
            println!();
            println!("Safely eject the iPod and check:");
            println!("  1. Does it boot normally?");
            println!("  2. Do tracks show album art in Now Playing?");
            println!("  3. Does Cover Flow work?");
        }
        Err(e) => {
            eprintln!("WRITE FAILED: {e}");
            std::process::exit(1);
        }
    }
}
