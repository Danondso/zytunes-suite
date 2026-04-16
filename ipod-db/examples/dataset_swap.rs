//! Diagnostic: swap individual datasets between original and our serialized iTunesDB.
//!
//! Creates candidate files in /tmp/swap-candidates/ with one dataset at a time
//! replaced by our serializer's output. Test each on the iPod to isolate which
//! dataset the firmware rejects.
//!
//! Usage:
//!   cargo run -p ipod-db --example dataset_swap --release

use std::path::PathBuf;

fn u32_at(data: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(data[off..off + 4].try_into().unwrap())
}

/// Extract dataset boundaries from an iTunesDB.
/// Returns Vec of (offset, size, type) for each mhsd.
fn find_datasets(data: &[u8]) -> Vec<(usize, usize, u32)> {
    let mhbd_hs = u32_at(data, 4) as usize;
    let num_ds = u32_at(data, 20);
    let mut datasets = Vec::new();
    let mut pos = mhbd_hs;
    for _ in 0..num_ds {
        if pos + 16 > data.len() || &data[pos..pos + 4] != b"mhsd" {
            break;
        }
        let ds_total = u32_at(data, pos + 8) as usize;
        let ds_type = u32_at(data, pos + 12);
        datasets.push((pos, ds_total, ds_type));
        pos += ds_total;
    }
    datasets
}

/// Rebuild an iTunesDB by replacing one dataset type with data from another DB.
/// Returns the new binary with updated mhbd total_size.
fn swap_dataset(base: &[u8], donor: &[u8], swap_type: u32) -> Vec<u8> {
    let base_ds = find_datasets(base);
    let donor_ds = find_datasets(donor);

    let mhbd_hs = u32_at(base, 4) as usize;
    let mut result = base[..mhbd_hs].to_vec(); // copy mhbd header

    for &(off, size, dtype) in &base_ds {
        if dtype == swap_type {
            // Use donor's dataset of this type.
            if let Some(&(d_off, d_size, _)) = donor_ds.iter().find(|d| d.2 == dtype) {
                result.extend_from_slice(&donor[d_off..d_off + d_size]);
            } else {
                // Donor doesn't have this type — keep original.
                result.extend_from_slice(&base[off..off + size]);
            }
        } else {
            result.extend_from_slice(&base[off..off + size]);
        }
    }

    // Patch total_size in mhbd header.
    let total = result.len() as u32;
    result[8..12].copy_from_slice(&total.to_le_bytes());
    result
}

fn main() {
    let orig_path = "/tmp/ipod-ref/iPod_Control/iTunes/iTunesDB";
    let original = std::fs::read(orig_path).unwrap();
    println!("Original: {} bytes", original.len());

    // Parse and serialize to get our output.
    let mount = PathBuf::from("/tmp/roundtrip-mount");
    std::fs::create_dir_all(mount.join("iPod_Control/iTunes")).unwrap();
    let db = ipod_db::itunesdb::parse(&original, mount).unwrap();
    let our_output = ipod_db::itunesdb_write::serialize(&db);
    println!("Our serializer: {} bytes", our_output.len());

    let fwid = ipod_db::hash::parse_firewire_id("000A2700215CDB22").unwrap();

    let out_dir = PathBuf::from("/tmp/swap-candidates");
    std::fs::create_dir_all(&out_dir).unwrap();

    // Original datasets.
    let orig_ds = find_datasets(&original);
    let our_ds = find_datasets(&our_output);
    println!("\nOriginal datasets:");
    for (off, size, dtype) in &orig_ds {
        println!("  type={dtype} offset={off} size={size}");
    }
    println!("Our datasets:");
    for (off, size, dtype) in &our_ds {
        println!("  type={dtype} offset={off} size={size}");
    }

    // Create candidates: original with ONE dataset swapped.
    let ds_names = [
        (4, "albums"),
        (1, "tracks"),
        (3, "podcasts"),
        (2, "playlists"),
        (5, "smart"),
    ];

    for &(dtype, name) in &ds_names {
        let mut candidate = swap_dataset(&original, &our_output, dtype);
        ipod_db::hash::sign_hash58(&mut candidate, &fwid).unwrap();
        let path = out_dir.join(format!("swap_{name}_type{dtype}.itdb"));
        std::fs::write(&path, &candidate).unwrap();
        println!(
            "\n{}: {} bytes (swapped type {})",
            path.display(),
            candidate.len(),
            dtype
        );
    }

    // Also create: original with ALL datasets swapped (= our full output + original header).
    let mut all_swapped = original[..u32_at(&original, 4) as usize].to_vec();
    let our_ds_list = find_datasets(&our_output);
    for &(off, size, _) in &our_ds_list {
        all_swapped.extend_from_slice(&our_output[off..off + size]);
    }
    let total = all_swapped.len() as u32;
    all_swapped[8..12].copy_from_slice(&total.to_le_bytes());
    ipod_db::hash::sign_hash58(&mut all_swapped, &fwid).unwrap();
    let path = out_dir.join("swap_all.itdb");
    std::fs::write(&path, &all_swapped).unwrap();
    println!(
        "\n{}: {} bytes (all datasets from our serializer, original header)",
        path.display(),
        all_swapped.len()
    );

    println!("\nTo test: copy one candidate to the iPod as iTunesDB, eject, and check.");
    println!("Start with swap_all.itdb — if that works, the issue was only the header.");
    println!("If not, try each individual swap to find the bad dataset.");
}
