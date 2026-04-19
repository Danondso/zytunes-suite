//! Dump every file under /Music on a connected Zune to a local directory,
//! mirroring the on-device folder structure. Skips files that already exist
//! locally with a matching size, so it's safe to re-run after interruption.
//!
//! ```
//! ZUNE_PID=063e cargo run --release -p zune-mtp --example dump_all -- /path/to/output
//! ```
//!
//! If no output dir is given, writes to `./zune-dump`.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use zune_mtp::container::{OperationCode, MTP_ROOT};
use zune_mtp::{mtpz, MtpSession, MtpzKeys};

const ASSOCIATION_FORMAT: u16 = 0x3001; // MTP association (directory) format

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let out_dir = PathBuf::from(
        args.get(1)
            .cloned()
            .unwrap_or_else(|| "zune-dump".to_string()),
    );

    let pid = std::env::var("ZUNE_PID")
        .ok()
        .and_then(|s| u16::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .unwrap_or(0x0710);

    println!("=== Zune media dump ===");
    println!("Target PID: 0x{pid:04x}");
    println!("Output dir: {}", out_dir.display());
    println!();

    if let Err(e) = fs::create_dir_all(&out_dir) {
        eprintln!("Cannot create {}: {e}", out_dir.display());
        std::process::exit(1);
    }

    print!("Opening USB session... ");
    let mut session = match MtpSession::open(0x045e, pid) {
        Ok(s) => {
            println!("ok");
            s
        }
        Err(e) => {
            println!("FAILED: {e}");
            std::process::exit(1);
        }
    };

    print!("Loading MTPZ keys... ");
    let keys = match MtpzKeys::load_default() {
        Ok(k) => {
            println!("ok");
            k
        }
        Err(e) => {
            println!("FAILED: {e}");
            std::process::exit(1);
        }
    };

    print!("MTPZ handshake... ");
    let log = |m: &str| eprintln!("  {m}");
    if let Err(e) = mtpz::authenticate(&mut session, &keys, &log) {
        println!("FAILED: {e}");
        std::process::exit(1);
    }
    println!("ok");

    let storage_id = match session.get_storage_ids() {
        Ok(ids) if !ids.is_empty() => ids[0],
        _ => {
            eprintln!("No storage found on device");
            std::process::exit(1);
        }
    };
    println!("Storage: {storage_id}");

    println!();
    println!("Walking device from root...");
    let start = Instant::now();
    let mut stats = Stats::default();
    walk(&mut session, storage_id, MTP_ROOT, &out_dir, &mut stats);
    let elapsed = start.elapsed();

    println!();
    println!("=== Done in {:?} ===", elapsed);
    println!("  Files pulled:   {}", stats.pulled);
    println!("  Files skipped:  {} (already present)", stats.skipped);
    println!("  Failed pulls:   {}", stats.failed);
    println!("  Bytes written:  {}", human_bytes(stats.bytes_written));
}

#[derive(Default)]
struct Stats {
    pulled: usize,
    skipped: usize,
    failed: usize,
    bytes_written: u64,
}

fn walk(
    session: &mut MtpSession,
    storage_id: u32,
    parent_handle: u32,
    local_dir: &Path,
    stats: &mut Stats,
) {
    let handles = match session.get_object_handles(storage_id, parent_handle) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("  (list {parent_handle} failed: {e})");
            return;
        }
    };

    for h in handles {
        let info = match session.get_object_info(h) {
            Ok(i) => i,
            Err(e) => {
                eprintln!("  (info {h} failed: {e})");
                continue;
            }
        };

        let child = local_dir.join(sanitize_filename(&info.filename));

        if info.object_format == ASSOCIATION_FORMAT {
            if let Err(e) = fs::create_dir_all(&child) {
                eprintln!("  mkdir {} failed: {e}", child.display());
                continue;
            }
            walk(session, storage_id, h, &child, stats);
            continue;
        }

        // Zero-byte virtual objects (AbstractAudioAlbum 0xBA03, Artist 0xB218,
        // etc.) have no file payload — they're metadata references. Skip.
        if info.compressed_size == 0 {
            stats.skipped += 1;
            continue;
        }

        // Already present with matching size? Skip.
        if let Ok(meta) = fs::metadata(&child) {
            if meta.len() == info.compressed_size as u64 {
                stats.skipped += 1;
                continue;
            }
        }

        print!("  {} ({} bytes)... ", child.display(), info.compressed_size);
        match session.execute_data_in(OperationCode::GetObject, &[h]) {
            Ok(bytes) => match fs::write(&child, &bytes) {
                Ok(()) => {
                    stats.pulled += 1;
                    stats.bytes_written += bytes.len() as u64;
                    println!("ok");
                }
                Err(e) => {
                    stats.failed += 1;
                    println!("WRITE FAIL: {e}");
                }
            },
            Err(e) => {
                stats.failed += 1;
                println!("PULL FAIL: {e}");
            }
        }
    }
}

/// Strip filesystem-hostile characters — Linux tolerates almost everything,
/// but `/` in a filename would silently create a nested path.
fn sanitize_filename(name: &str) -> String {
    name.replace('/', "_")
}

fn human_bytes(n: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    format!("{:.2} {}", v, UNITS[u])
}
