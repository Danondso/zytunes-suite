//! Isolation test: replace one dataset at a time in the original DB to find
//! which part of our serialization the firmware rejects.
//!
//! Modes:
//!   tracks    — replace only mhsd type=1 (tracks) with our version
//!   playlists — replace only mhsd type=2+3 (playlists) with our version
//!   full      — replace everything (same as write_test)
//!
//! Usage:
//!   sudo ./target/debug/examples/isolate_test /mnt/ipod-classic tracks

use ipod_db::{hash, itunesdb, itunesdb_write};
use std::io::Write;

fn main() {
    let mount = std::env::args()
        .nth(1)
        .expect("usage: isolate_test <mount> <mode>");
    let mode = std::env::args().nth(2).unwrap_or_else(|| "tracks".into());

    let db_path = format!("{mount}/iPod_Control/iTunes/iTunesDB");
    let backup_path = format!("{mount}/iPod_Control/iTunes/iTunesDB.original");

    let original = std::fs::read(&backup_path).expect("Need .original backup");
    println!("Original: {} bytes", original.len());

    // Parse original to get track/playlist data
    let db = itunesdb::parse(&original, mount.clone().into()).unwrap();
    println!(
        "Parsed: {} tracks, {} playlists",
        db.tracks.len(),
        db.playlists.len()
    );

    // Extract datasets from original
    let orig_datasets = extract_datasets(&original);
    println!(
        "Original datasets: {:?}",
        orig_datasets.iter().map(|(t, _)| *t).collect::<Vec<_>>()
    );

    // Serialize our version and extract its datasets
    let our_serialized = itunesdb_write::serialize(&db);
    let our_datasets = extract_datasets(&our_serialized);
    println!(
        "Our datasets: {:?}",
        our_datasets.iter().map(|(t, _)| *t).collect::<Vec<_>>()
    );

    // Build hybrid DB: original mhbd header + mix of original/our datasets
    let mhbd_hdr_size = u32::from_le_bytes(original[4..8].try_into().unwrap()) as usize;
    let mut result = original[..mhbd_hdr_size].to_vec(); // original mhbd header

    let our_ds_map: std::collections::HashMap<u32, &[u8]> = our_datasets
        .iter()
        .map(|(t, d)| (*t, d.as_slice()))
        .collect();

    let mut num_datasets = 0u32;
    for &(ds_type, ref ds_data) in &orig_datasets {
        match mode.as_str() {
            "tracks" => {
                if ds_type == 1 {
                    if let Some(our) = our_ds_map.get(&1) {
                        println!(
                            "Replacing mhsd type=1 (tracks): {} -> {} bytes",
                            ds_data.len(),
                            our.len()
                        );
                        result.extend_from_slice(our);
                    } else {
                        result.extend_from_slice(ds_data);
                    }
                } else {
                    result.extend_from_slice(ds_data);
                }
            }
            "playlists" => {
                if ds_type == 2 || ds_type == 3 {
                    if let Some(our) = our_ds_map.get(&ds_type) {
                        println!(
                            "Replacing mhsd type={ds_type}: {} -> {} bytes",
                            ds_data.len(),
                            our.len()
                        );
                        result.extend_from_slice(our);
                    } else {
                        result.extend_from_slice(ds_data);
                    }
                } else {
                    result.extend_from_slice(ds_data);
                }
            }
            "full" => {
                if let Some(our) = our_ds_map.get(&ds_type) {
                    println!(
                        "Replacing mhsd type={ds_type}: {} -> {} bytes",
                        ds_data.len(),
                        our.len()
                    );
                    result.extend_from_slice(our);
                } else {
                    result.extend_from_slice(ds_data);
                }
            }
            _ => {
                eprintln!("Unknown mode: {mode}. Use: tracks, playlists, full");
                std::process::exit(1);
            }
        }
        num_datasets += 1;
    }

    // Fix mhbd total_size and num_datasets
    let total = result.len() as u32;
    result[8..12].copy_from_slice(&total.to_le_bytes());
    result[20..24].copy_from_slice(&num_datasets.to_le_bytes());

    // Re-sign hash
    let fwid = std::env::args()
        .nth(3)
        .or_else(|| {
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
        })
        .expect("Need FirewireGuid");

    let firewire_id = hash::parse_firewire_id(&fwid).unwrap();
    hash::sign_hash58(&mut result, &firewire_id).unwrap();

    println!("\nMode: {mode}");
    println!(
        "Output: {} bytes (original was {})",
        result.len(),
        original.len()
    );

    print!("Write? [y/N] ");
    std::io::stdout().flush().unwrap();
    let mut input = String::new();
    std::io::stdin().read_line(&mut input).unwrap();
    if input.trim().to_lowercase() != "y" {
        println!("Aborted.");
        return;
    }

    std::fs::write(&db_path, &result).unwrap();
    println!("Written. Eject and test.");
}

/// Extract (type, raw_bytes) for each mhsd dataset in an iTunesDB.
fn extract_datasets(data: &[u8]) -> Vec<(u32, Vec<u8>)> {
    let mhbd_hdr = u32::from_le_bytes(data[4..8].try_into().unwrap()) as usize;
    let num_ds = u32::from_le_bytes(data[20..24].try_into().unwrap());
    let mut datasets = Vec::new();
    let mut pos = mhbd_hdr;
    for _ in 0..num_ds {
        if pos + 16 > data.len() {
            break;
        }
        let ds_type = u32::from_le_bytes(data[pos + 12..pos + 16].try_into().unwrap());
        let ds_total = u32::from_le_bytes(data[pos + 8..pos + 12].try_into().unwrap()) as usize;
        if pos + ds_total > data.len() {
            break;
        }
        datasets.push((ds_type, data[pos..pos + ds_total].to_vec()));
        pos += ds_total;
    }
    datasets
}
