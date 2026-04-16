//! Diagnostic: patch artwork flags in-place on the original iTunesDB binary.
//!
//! This modifies ONLY the artwork_count (+132) and has_artwork (+156) fields
//! in each mhit, then re-signs hash58. Everything else is byte-identical to
//! the original. If this works on the iPod but our full serialize doesn't,
//! the problem is in the rebuilt datasets.
//!
//! Usage:
//!   cargo run -p ipod-db --example patch_artwork_inplace --release -- /mnt/ipod-classic <artwork_count>

fn u32_at(data: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(data[off..off + 4].try_into().unwrap())
}

fn put_u32(data: &mut [u8], off: usize, val: u32) {
    data[off..off + 4].copy_from_slice(&val.to_le_bytes());
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("Usage: patch_artwork_inplace <mount_path> <artwork_count>");
        std::process::exit(1);
    }

    let mount = std::path::PathBuf::from(&args[1]);
    let art_count: u32 = args[2].parse().unwrap();
    let db_path = mount.join("iPod_Control/iTunes/iTunesDB");

    let mut data = std::fs::read(&db_path).unwrap();
    println!("Read {} bytes from {}", data.len(), db_path.display());

    // Walk mhbd → find mhsd type=1 → walk mhit records.
    let mhbd_hs = u32_at(&data, 4) as usize;
    let num_datasets = u32_at(&data, 20);

    let mut cursor = mhbd_hs;
    let mut patched = 0u32;
    for _ in 0..num_datasets {
        if cursor + 16 > data.len() || &data[cursor..cursor + 4] != b"mhsd" {
            break;
        }
        let ds_hs = u32_at(&data, cursor + 4) as usize;
        let ds_total = u32_at(&data, cursor + 8) as usize;
        let ds_type = u32_at(&data, cursor + 12);

        if ds_type == 1 {
            let list_start = cursor + ds_hs;
            let list_hs = u32_at(&data, list_start + 4) as usize;
            let count = u32_at(&data, list_start + 8);

            let mut pos = list_start + list_hs;
            for _ in 0..count {
                if pos + 160 > data.len() || &data[pos..pos + 4] != b"mhit" {
                    break;
                }
                let mhit_hs = u32_at(&data, pos + 4) as usize;
                let mhit_total = u32_at(&data, pos + 8) as usize;

                if mhit_hs >= 160 {
                    // Patch artwork_count at +132 and has_artwork at +156.
                    put_u32(&mut data, pos + 132, art_count);
                    put_u32(&mut data, pos + 156, if art_count > 0 { 1 } else { 0 });
                    patched += 1;
                }

                pos += mhit_total;
            }
        }

        cursor += ds_total;
    }

    println!("Patched {patched} tracks: artwork_count={art_count}");

    // Backup.
    let backup = db_path.with_extension("bak");
    std::fs::copy(&db_path, &backup).unwrap();
    println!("Backup: {}", backup.display());

    // Re-sign hash58.
    let fwid_str = std::process::Command::new("lsusb")
        .args(["-v", "-d", "05ac:"])
        .output()
        .ok()
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .find(|l| l.contains("iSerial"))
                .and_then(|l| l.split_whitespace().last().map(|s| s.to_string()))
        })
        .unwrap_or_else(|| "000A2700215CDB22".to_string());

    let fwid = ipod_db::hash::parse_firewire_id(&fwid_str).unwrap();
    ipod_db::hash::sign_hash58(&mut data, &fwid).unwrap();
    println!("Signed with hash58 (FirewireGuid: {fwid_str})");

    // Write.
    std::fs::write(&db_path, &data).unwrap();
    println!("Written. Eject and test.");
}
