//! Write a round-tripped iTunesDB back to the iPod for firmware validation.
//!
//! Parses the existing database, re-serializes it (existing tracks use raw
//! header replay, preserving all fields), and writes back with hash58 signing.
//!
//! Optionally adds a new test track (--add-test-track) to exercise the
//! from-scratch mhit path alongside raw-replayed tracks.
//!
//! Usage:
//!   cargo run -p ipod-db --example write_roundtrip --release -- <mount_path> <firewire_guid>
//!   cargo run -p ipod-db --example write_roundtrip --release -- <mount_path> <firewire_guid> --add-test-track

use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("Usage: write_roundtrip <mount_path> <firewire_guid> [options]");
        eprintln!("  mount_path:       iPod mount (e.g. /Volumes/IPOD)");
        eprintln!("  firewire_guid:    device serial (e.g. 000A2700215CDB22)");
        eprintln!("  --add-test-track: add a dummy track to test from-scratch mhit");
        eprintln!("  --rebuild-from N: clear raw headers on tracks with track_id >= N");
        std::process::exit(1);
    }

    let mount = PathBuf::from(&args[1]);
    let fwid_str = &args[2];
    let add_test_track = args.iter().any(|a| a == "--add-test-track");
    let rebuild_from: Option<u32> = args
        .windows(2)
        .find(|w| w[0] == "--rebuild-from")
        .and_then(|w| w[1].parse().ok());

    let db_path = mount.join("iPod_Control/iTunes/iTunesDB");
    let original = std::fs::read(&db_path).unwrap();
    println!("Read {} bytes from {}", original.len(), db_path.display());

    // Parse.
    let mut db = ipod_db::itunesdb::parse(&original, mount.clone()).unwrap();
    println!(
        "Parsed: {} tracks, {} playlists",
        db.tracks.len(),
        db.playlists.len()
    );

    let orig_track_count = db.tracks.len();

    // Optionally clear raw headers to force from-scratch rebuild.
    if let Some(min_tid) = rebuild_from {
        let mut cleared = 0;
        for track in &mut db.tracks {
            if track.track_id >= min_tid {
                track.clear_raw_header();
                cleared += 1;
            }
        }
        println!(
            "Cleared raw headers on {} tracks (will rebuild from scratch)",
            cleared
        );
    }

    if add_test_track {
        // Find a free F-dir slot.
        let test_path = ":iPod_Control:Music:F00:ZTEST.mp3".to_string();
        let mut track = ipod_db::IpodTrack::default();
        track.title = "Test Track (zytunes)".into();
        track.artist = "Test Artist".into();
        track.album = "Test Album".into();
        track.genre = Some("Test".into());
        track.track_number = Some(1);
        track.disc_number = Some(1);
        track.total_time_ms = Some(180000);
        track.year = Some(2026);
        track.file_size = 4_500_000;
        track.bitrate = Some(320);
        track.sample_rate = Some(44100);
        track.ipod_path = test_path;
        track.filetype = 0x4d503320; // MP3
        let dbid = db.add_track(track);
        println!(
            "Added test track (dbid=0x{:016x}), total tracks: {}",
            dbid,
            db.tracks.len()
        );
    }

    // Serialize.
    let mut output = ipod_db::itunesdb_write::serialize(&db);
    println!("Serialized: {} bytes", output.len());

    // Sign.
    let fwid = ipod_db::hash::parse_firewire_id(fwid_str).unwrap();
    ipod_db::hash::sign_hash58(&mut output, &fwid).unwrap();
    println!("Signed with hash58 (FirewireGuid: {})", fwid_str);

    // Backup existing.
    let backup = db_path.with_extension("pre-roundtrip-bak");
    std::fs::copy(&db_path, &backup).unwrap();
    println!("Backup: {}", backup.display());

    // Write.
    std::fs::write(&db_path, &output).unwrap();
    println!("Written {} bytes to {}", output.len(), db_path.display());
    println!(
        "\nOriginal: {} tracks → Output: {} tracks",
        orig_track_count,
        db.tracks.len()
    );
    println!("Eject the iPod and verify on device.");
    if add_test_track {
        println!("Look for 'Test Track (zytunes)' by 'Test Artist' in the library.");
        println!("NOTE: The test track has no actual audio file — it may show but not play.");
    }
}
