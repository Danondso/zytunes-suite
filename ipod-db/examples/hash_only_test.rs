//! Isolation test: write the original DB back with only the hash recomputed.
//! If the iPod accepts this, the hash algorithm is correct and the problem
//! is in our serialization. If it rejects it, the hash is wrong.
//!
//! Usage:
//!   sudo ./target/debug/examples/hash_only_test /mnt/ipod-classic 000A2700215CDB22

use ipod_db::hash;
use std::io::Write;

fn main() {
    let mount = std::env::args()
        .nth(1)
        .expect("usage: hash_only_test <mount> [firewire_id]");
    let fwid = std::env::args().nth(2).or_else(|| {
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

    let fwid = fwid.expect("Could not detect FirewireGuid. Pass as second argument.");

    let db_path = format!("{mount}/iPod_Control/iTunes/iTunesDB");
    let backup_path = format!("{mount}/iPod_Control/iTunes/iTunesDB.original");

    // Read original
    let original = std::fs::read(&backup_path)
        .or_else(|_| std::fs::read(&db_path))
        .expect("Could not read iTunesDB");

    println!("Original size: {} bytes", original.len());

    // Copy and re-sign
    let mut data = original.clone();
    let firewire_id = hash::parse_firewire_id(&fwid).unwrap();

    // Show original hash
    let orig_hash: Vec<u8> = data[0x58..0x6C].to_vec();
    println!(
        "Original hash: {}",
        orig_hash
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );

    hash::sign_hash58(&mut data, &firewire_id).unwrap();

    let new_hash: Vec<u8> = data[0x58..0x6C].to_vec();
    println!(
        "New hash:      {}",
        new_hash
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    println!(
        "Hashes match:  {}",
        if orig_hash == new_hash { "YES" } else { "NO" }
    );

    if data == original {
        println!("\nBytes identical to original. Nothing to test — the hash was already correct.");
        return;
    }

    // The only difference should be the hash bytes (if any)
    let mut diffs = 0;
    for (i, (a, b)) in original.iter().zip(data.iter()).enumerate() {
        if a != b {
            if diffs < 5 {
                println!("  diff at offset 0x{i:04x}: 0x{a:02x} -> 0x{b:02x}");
            }
            diffs += 1;
        }
    }
    println!("Total bytes changed: {diffs}");

    print!("\nWrite re-signed original to device? [y/N] ");
    std::io::stdout().flush().unwrap();
    let mut input = String::new();
    std::io::stdin().read_line(&mut input).unwrap();
    if input.trim().to_lowercase() != "y" {
        println!("Aborted.");
        return;
    }

    std::fs::write(&db_path, &data).unwrap();
    println!("Written. Eject and test.");
}
