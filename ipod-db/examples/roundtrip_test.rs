//! Offline iTunesDB round-trip test: parse → serialize → re-sign → compare.
//!
//! Usage:
//!   cargo run -p ipod-db --example roundtrip_test --release -- /tmp/ipod-ref/iPod_Control/iTunes/iTunesDB
//!
//! Compares the re-serialized output against the original by running
//! structural checks: total size, dataset types/sizes, track count,
//! mhod type distribution, playlist structure.

use std::path::PathBuf;

fn u32_at(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap())
}

fn u64_at(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap())
}

/// Lightweight structural summary of an iTunesDB.
#[derive(Debug)]
struct DbSummary {
    total_size: usize,
    db_version: u32,
    db_id: u64,
    num_datasets: u32,
    datasets: Vec<DatasetSummary>,
    track_count: u32,
    /// mhod type → count across all tracks
    track_mhod_types: std::collections::BTreeMap<u32, u32>,
    playlist_count: u32,
}

#[derive(Debug)]
struct DatasetSummary {
    ds_type: u32,
    total_size: u32,
}

fn summarize(data: &[u8]) -> DbSummary {
    let total_size = data.len();
    let mhbd_header_size = u32_at(data, 4) as usize;
    let db_version = u32_at(data, 16);
    let num_datasets = u32_at(data, 20);
    let db_id = u64_at(data, 24);

    let mut datasets = Vec::new();
    let mut track_count = 0u32;
    let mut track_mhod_types = std::collections::BTreeMap::new();
    let mut playlist_count = 0u32;

    let mut cursor = mhbd_header_size;
    for _ in 0..num_datasets {
        if cursor + 16 > data.len() || &data[cursor..cursor + 4] != b"mhsd" {
            break;
        }
        let ds_type = u32_at(data, cursor + 12);
        let ds_total = u32_at(data, cursor + 8);
        datasets.push(DatasetSummary {
            ds_type,
            total_size: ds_total,
        });

        let mhsd_header_size = u32_at(data, cursor + 4) as usize;
        let list_start = cursor + mhsd_header_size;

        if ds_type == 1
            && list_start + 12 <= data.len()
            && &data[list_start..list_start + 4] == b"mhlt"
        {
            let list_header = u32_at(data, list_start + 4) as usize;
            let count = u32_at(data, list_start + 8);
            track_count = count;

            // Walk mhit records to count mhod types.
            let mut pos = list_start + list_header;
            for _ in 0..count {
                if pos + 16 > data.len() || &data[pos..pos + 4] != b"mhit" {
                    break;
                }
                let mhit_header = u32_at(data, pos + 4) as usize;
                let mhit_total = u32_at(data, pos + 8) as usize;
                let mhit_num_mhods = u32_at(data, pos + 12);

                let mut mhod_pos = pos + mhit_header;
                for _ in 0..mhit_num_mhods {
                    if mhod_pos + 12 > data.len() || &data[mhod_pos..mhod_pos + 4] != b"mhod" {
                        break;
                    }
                    let mhod_total = u32_at(data, mhod_pos + 8) as usize;
                    let mhod_type = u32_at(data, mhod_pos + 12);
                    *track_mhod_types.entry(mhod_type).or_insert(0) += 1;
                    mhod_pos += mhod_total;
                }

                pos += mhit_total;
            }
        } else if (ds_type == 2 || ds_type == 3)
            && list_start + 12 <= data.len()
            && &data[list_start..list_start + 4] == b"mhlp"
        {
            if ds_type == 2 {
                playlist_count = u32_at(data, list_start + 8);
            }
        }

        cursor += ds_total as usize;
    }

    DbSummary {
        total_size,
        db_version,
        db_id,
        num_datasets,
        datasets,
        track_count,
        track_mhod_types,
        playlist_count,
    }
}

fn print_summary(label: &str, s: &DbSummary) {
    println!("=== {} ===", label);
    println!("  size:         {} bytes", s.total_size);
    println!("  db_version:   0x{:x}", s.db_version);
    println!("  db_id:        0x{:016x}", s.db_id);
    println!("  num_datasets: {}", s.num_datasets);
    for (i, ds) in s.datasets.iter().enumerate() {
        println!("  mhsd[{}]: type={} size={}", i, ds.ds_type, ds.total_size);
    }
    println!("  tracks:       {}", s.track_count);
    println!("  playlists:    {}", s.playlist_count);
    println!("  track mhod types:");
    for (t, c) in &s.track_mhod_types {
        println!("    type {:>3}: {}", t, c);
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: roundtrip_test <itunesdb_path>");
        std::process::exit(1);
    }

    let db_path = PathBuf::from(&args[1]);
    let original = std::fs::read(&db_path).unwrap();
    println!("Original: {} ({} bytes)", db_path.display(), original.len());

    // Parse.
    let mount = PathBuf::from("/tmp/roundtrip-mount");
    std::fs::create_dir_all(mount.join("iPod_Control/iTunes")).unwrap();

    let db = ipod_db::itunesdb::parse(&original, mount.clone()).unwrap();
    println!(
        "Parsed: {} tracks, {} playlists",
        db.tracks.len(),
        db.playlists.len()
    );

    // Serialize.
    let mut output = ipod_db::itunesdb_write::serialize(&db);

    // Re-sign with the same FirewireGuid.
    let fwid = ipod_db::hash::parse_firewire_id("000A2700215CDB22").unwrap();
    ipod_db::hash::sign_hash58(&mut output, &fwid).unwrap();

    // Save for inspection.
    let out_path = "/tmp/roundtrip-output.itdb";
    std::fs::write(out_path, &output).unwrap();
    println!("Output:   {} ({} bytes)", out_path, output.len());
    println!();

    // Compare.
    let orig_summary = summarize(&original);
    let out_summary = summarize(&output);

    print_summary("ORIGINAL", &orig_summary);
    println!();
    print_summary("ROUND-TRIP", &out_summary);

    // Diff report.
    println!("\n=== DIFFS ===");
    let mut diffs = 0;

    if orig_summary.total_size != out_summary.total_size {
        println!(
            "  SIZE: {} → {} (delta {})",
            orig_summary.total_size,
            out_summary.total_size,
            out_summary.total_size as i64 - orig_summary.total_size as i64
        );
        diffs += 1;
    }
    if orig_summary.db_id != out_summary.db_id {
        println!(
            "  DB_ID: 0x{:016x} → 0x{:016x}",
            orig_summary.db_id, out_summary.db_id
        );
        diffs += 1;
    }
    if orig_summary.num_datasets != out_summary.num_datasets {
        println!(
            "  NUM_DATASETS: {} → {}",
            orig_summary.num_datasets, out_summary.num_datasets
        );
        diffs += 1;
    }
    if orig_summary.track_count != out_summary.track_count {
        println!(
            "  TRACK_COUNT: {} → {}",
            orig_summary.track_count, out_summary.track_count
        );
        diffs += 1;
    }
    if orig_summary.playlist_count != out_summary.playlist_count {
        println!(
            "  PLAYLIST_COUNT: {} → {}",
            orig_summary.playlist_count, out_summary.playlist_count
        );
        diffs += 1;
    }

    // Dataset type/size diffs.
    for (i, (a, b)) in orig_summary
        .datasets
        .iter()
        .zip(out_summary.datasets.iter())
        .enumerate()
    {
        if a.ds_type != b.ds_type {
            println!("  DATASET[{}] TYPE: {} → {}", i, a.ds_type, b.ds_type);
            diffs += 1;
        }
        if a.total_size != b.total_size {
            println!(
                "  DATASET[{}] (type={}) SIZE: {} → {} (delta {})",
                i,
                a.ds_type,
                a.total_size,
                b.total_size,
                b.total_size as i64 - a.total_size as i64
            );
            diffs += 1;
        }
    }

    // Track mhod type diffs.
    let all_types: std::collections::BTreeSet<u32> = orig_summary
        .track_mhod_types
        .keys()
        .chain(out_summary.track_mhod_types.keys())
        .copied()
        .collect();
    for t in all_types {
        let a = orig_summary.track_mhod_types.get(&t).copied().unwrap_or(0);
        let b = out_summary.track_mhod_types.get(&t).copied().unwrap_or(0);
        if a != b {
            println!("  TRACK_MHOD type={}: {} → {}", t, a, b);
            diffs += 1;
        }
    }

    if diffs == 0 {
        println!("  (none — perfect round-trip!)");
    } else {
        println!("\n  {} differences found", diffs);
    }
}
