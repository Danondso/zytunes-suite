//! Device write test: parse a real iTunesDB, serialize with our writer, write back.
//!
//! Usage:
//!   cargo run -p ipod-db --example write_test -- /mnt/ipod-classic
//!
//! This will:
//!   1. Back up the original iTunesDB to iTunesDB.original
//!   2. Parse the original database
//!   3. Serialize it with our writer
//!   4. Validate the output (structural check + round-trip)
//!   5. Ask for confirmation before writing
//!   6. Write the new iTunesDB
//!
//! To restore: copy iTunesDB.original back to iTunesDB

use ipod_db::{hash, itunesdb, itunesdb_write};
use std::path::PathBuf;

fn main() {
    let mount = match std::env::args().nth(1) {
        Some(p) => PathBuf::from(p),
        None => {
            eprintln!("Usage: write_test <mount_path>");
            std::process::exit(1);
        }
    };

    let itunes_dir = mount.join("iPod_Control").join("iTunes");
    let db_path = itunes_dir.join("iTunesDB");
    let backup_path = itunes_dir.join("iTunesDB.original");

    if !db_path.exists() {
        eprintln!("ERROR: no iTunesDB at {}", db_path.display());
        std::process::exit(1);
    }

    // Step 1: Read original
    println!("Reading original iTunesDB...");
    let original = std::fs::read(&db_path).unwrap();
    println!("  Size: {} bytes", original.len());

    // Step 2: Parse
    println!("Parsing...");
    let db = match itunesdb::parse(&original, mount.clone()) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("PARSE FAILED: {e}");
            std::process::exit(1);
        }
    };
    println!(
        "  {} tracks, {} playlists, db version 0x{:x}",
        db.tracks.len(),
        db.playlists.len(),
        db.db_version
    );

    // Step 3: Serialize
    println!("Serializing with our writer...");
    let mut serialized = itunesdb_write::serialize(&db);
    println!("  Size: {} bytes", serialized.len());

    // Step 3a: Graft original mhbd header fields onto our output.
    // Our serializer writes a mostly-zero 244-byte header. The firmware needs
    // fields like platform, db_id, language_id, and persistent_id to be populated.
    let orig_hdr_size = u32::from_le_bytes(original[4..8].try_into().unwrap()) as usize;
    let our_hdr_size = u32::from_le_bytes(serialized[4..8].try_into().unwrap()) as usize;
    if orig_hdr_size == our_hdr_size {
        // Copy non-structural fields from original header (skip magic, sizes, version, datasets).
        // Preserve: +24 db_id, +32..+48 platform/language/ids, +0x46..+0x58 persistent_id/etc,
        // +0x6C..end timezone/language/etc. Skip: +0x58 hash (will be recomputed).
        for &(start, end) in &[(24, 48), (0x46, 0x58), (0x6C, orig_hdr_size)] {
            serialized[start..end].copy_from_slice(&original[start..end]);
        }
        println!("  Grafted original mhbd header fields");
    }

    // Step 3b: Sign with hash58 (required for iPod Classic)
    let firewire_id = std::env::args().nth(2).or_else(|| {
        // Try to get from lsusb for Apple devices
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

    if let Some(ref fwid) = firewire_id {
        println!("Signing with hash58 (FirewireGuid: {fwid})...");
        let id = hash::parse_firewire_id(fwid).unwrap();
        hash::sign_hash58(&mut serialized, &id).unwrap();
        println!("  Hash written at mhbd offset 0x58");
    } else {
        println!("WARNING: No FirewireGuid found — skipping hash58 signing.");
        println!("  The iPod Classic will reject an unsigned database.");
        println!("  Pass the FirewireGuid as a second argument, or connect via USB.");
    }

    // Step 4: Validate — can we parse our own output?
    println!("Validating serialized output...");
    let db2 = match itunesdb::parse(&serialized, mount.clone()) {
        Ok(db2) => db2,
        Err(e) => {
            eprintln!("SELF-VALIDATION FAILED: {e}");
            eprintln!("Our serializer produced output we can't parse. Aborting.");
            std::process::exit(1);
        }
    };

    if db2.tracks.len() != db.tracks.len() {
        eprintln!(
            "TRACK COUNT MISMATCH: {} vs {}. Aborting.",
            db.tracks.len(),
            db2.tracks.len()
        );
        std::process::exit(1);
    }
    if db2.playlists.len() != db.playlists.len() {
        eprintln!(
            "PLAYLIST COUNT MISMATCH: {} vs {}. Aborting.",
            db.playlists.len(),
            db2.playlists.len()
        );
        std::process::exit(1);
    }

    // Spot-check every track's critical fields
    let mut mismatches = 0;
    for (a, b) in db.tracks.iter().zip(db2.tracks.iter()) {
        if a.title != b.title
            || a.artist != b.artist
            || a.ipod_path != b.ipod_path
            || a.dbid != b.dbid
        {
            if mismatches < 3 {
                eprintln!(
                    "  MISMATCH: \"{}\" vs \"{}\" (dbid {} vs {})",
                    a.title, b.title, a.dbid, b.dbid
                );
            }
            mismatches += 1;
        }
    }
    if mismatches > 0 {
        eprintln!("{mismatches} track(s) have field mismatches. Aborting.");
        std::process::exit(1);
    }

    println!(
        "  Validation PASSED: all {} tracks and {} playlists match",
        db2.tracks.len(),
        db2.playlists.len()
    );

    // Step 5: Backup
    if backup_path.exists() {
        println!("  Backup already exists at {}", backup_path.display());
    } else {
        println!("  Backing up to {}...", backup_path.display());
        std::fs::copy(&db_path, &backup_path).unwrap();
        println!("  Backup created ({} bytes)", original.len());
    }

    // Step 6: Confirm
    println!();
    println!(
        "Ready to write {} bytes to {}",
        serialized.len(),
        db_path.display()
    );
    println!(
        "Original was {} bytes ({} byte difference)",
        original.len(),
        (original.len() as i64 - serialized.len() as i64).abs()
    );
    println!();
    println!(
        "To restore: cp {} {}",
        backup_path.display(),
        db_path.display()
    );
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

    // Step 7: Write
    println!("Writing...");
    std::fs::write(&db_path, &serialized).unwrap();
    println!("Done! Wrote {} bytes.", serialized.len());
    println!();
    println!("Now safely eject the iPod and check:");
    println!("  1. Does it boot to the main menu?");
    println!("  2. Can you browse Artists / Albums?");
    println!("  3. Can you play a track?");
    println!();
    println!("If anything is wrong:");
    println!("  1. Reconnect and mount read-write");
    println!("  2. cp {} {}", backup_path.display(), db_path.display());
    println!("  3. Eject and reboot the iPod");
}
