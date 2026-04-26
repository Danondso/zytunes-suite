//! Diagnostic: dump every non-zero field in the mhit extended region (+212..+624).
//!
//! Reads a reference iTunesDB and, for each mhit, records the u32 value at every
//! 4-byte-aligned offset in the extended region. Then prints a summary showing:
//! - Which offsets are consistently non-zero across all tracks
//! - The range of values seen at each offset
//! - Which offsets vary per-track vs are constant
//!
//! This tells us exactly which fields we need to populate in the from-scratch writer.
//!
//! Usage:
//!   cargo run -p ipod-db --example mhit_field_dump --release -- <iTunesDB_path>
//!
//! Optional: pass a second path to diff two databases (e.g. original vs our output):
//!   cargo run -p ipod-db --example mhit_field_dump --release -- <reference> <ours>

use std::collections::BTreeMap;
use std::path::PathBuf;

fn u32_at(data: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(data[off..off + 4].try_into().unwrap())
}

fn u64_at(data: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(data[off..off + 8].try_into().unwrap())
}

/// Known field names for mhit offsets (from libgpod, iPodLinux wiki, and device testing).
fn field_name(offset: usize) -> &'static str {
    match offset {
        0 => "magic",
        4 => "header_size",
        8 => "total_size",
        12 => "num_mhods",
        16 => "track_id",
        20 => "visible",
        24 => "filetype",
        28 => "type/compilation/rating",
        32 => "date_modified",
        36 => "file_size",
        40 => "total_time_ms",
        44 => "track_number",
        48 => "total_tracks",
        52 => "year",
        56 => "bitrate",
        60 => "sample_rate",
        64 => "volume_adjust",
        68 => "start_time",
        72 => "stop_time",
        76 => "sound_check",
        80 => "play_count",
        84 => "last_played",
        88 => "date_added_to_device",
        92 => "disc_number",
        96 => "disc_total",
        100 => "sort_order",
        104 => "date_added",
        108 => "date_released",
        112 => "dbid_lo",
        116 => "dbid_hi",
        120 => "checked",
        124 => "app_rating",
        128 => "bpm",
        132 => "artwork_count",
        136 => "sample_rate_dup",
        140 => "date_released2",
        144 => "explicit_flag",
        148 => "skip_count",
        152 => "last_skipped",
        156 => "has_artwork",
        160 => "skip_shuffling",
        164 => "remember_playback_pos",
        168 => "dbid2_lo",
        172 => "dbid2_hi",
        176 => "lyrics_flag",
        180 => "movie_flag",
        184 => "mark_unplayed",
        188 => "size_on_disk",
        192 => "date_modified2",
        196 => "per_track_hash",
        200 => "media_type_legacy",
        204 => "season_or_episode",
        208 => "has_gapless_data",
        // Extended region (+212..+624) — less documented
        212 => "unk_212",
        216 => "gapless_encoding_data",
        220 => "unk_220",
        224 => "unk_224",
        228 => "unk_228",
        232 => "unk_232",
        236 => "unk_236",
        240 => "unk_240",
        244 => "unk_244",
        248 => "gapless_encoding_drain",
        252 => "gapless_encoding_delay",
        256 => "has_gapless_encoding",
        260 => "unk_260",
        264 => "unk_264",
        268 => "unk_268",
        272 => "unk_272",
        276 => "unk_276",
        280 => "unk_280",
        284 => "unk_284",
        288 => "album_id",
        292 => "unk_292 (const?)",
        296 => "unk_296 (const?)",
        300 => "secondary_id",
        304 => "unk_304",
        308 => "media_type_detailed",
        312 => "unk_312",
        316 => "unk_316",
        320 => "unk_320",
        324 => "unk_324",
        328 => "unk_328",
        332 => "unk_332",
        336 => "unk_336",
        340 => "unk_340",
        344 => "unk_344",
        348 => "unk_348",
        352 => "unk_352",
        356 => "unk_356",
        360 => "unk_360",
        364 => "unk_364",
        368 => "unk_368",
        372 => "unk_372",
        376 => "unk_376",
        380 => "unk_380",
        384 => "unk_384",
        388 => "unk_388",
        392 => "unk_392",
        396 => "unk_396",
        400 => "unk_400",
        404 => "unk_404",
        408 => "unk_408",
        412 => "unk_412",
        416 => "unk_416",
        420 => "unk_420",
        424 => "unk_424",
        428 => "unk_428",
        432 => "unk_432",
        436 => "unk_436",
        440 => "unk_440",
        444 => "unk_444",
        448 => "unk_448",
        452 => "unk_452",
        456 => "unk_456",
        460 => "unk_460",
        464 => "unk_464",
        468 => "unk_468",
        472 => "unk_472",
        476 => "unk_476",
        480 => "artist_id",
        484 => "unk_484",
        488 => "unk_488",
        492 => "unk_492",
        496 => "unk_496",
        500 => "unk_500 (track_ref?)",
        504 => "unk_504",
        508 => "unk_508",
        512 => "unk_512",
        516 => "unk_516",
        520 => "unk_520",
        524 => "media_type",
        528 => "unk_528",
        532 => "unk_532",
        536 => "unk_536",
        540 => "unk_540",
        544 => "unk_544",
        548 => "unk_548",
        552 => "unk_552",
        556 => "unk_556",
        560 => "unk_560",
        564 => "unk_564",
        568 => "unk_568",
        572 => "unk_572",
        576 => "unk_576",
        580 => "unk_580",
        584 => "unk_584",
        588 => "unk_588",
        592 => "unk_592",
        596 => "unk_596",
        600 => "unk_600",
        604 => "unk_604",
        608 => "unk_608",
        612 => "unk_612",
        616 => "unk_616",
        620 => "unk_620",
        _ => "???",
    }
}

/// Extract mhit raw headers from an iTunesDB binary.
/// Returns Vec of (track_id, dbid, raw_header_bytes).
fn extract_mhit_headers(data: &[u8]) -> Vec<(u32, u64, Vec<u8>)> {
    let mhbd_hs = u32_at(data, 4) as usize;
    let num_datasets = u32_at(data, 20);

    let mut results = Vec::new();
    let mut cursor = mhbd_hs;

    for _ in 0..num_datasets {
        if cursor + 16 > data.len() || &data[cursor..cursor + 4] != b"mhsd" {
            break;
        }
        let ds_hs = u32_at(data, cursor + 4) as usize;
        let ds_total = u32_at(data, cursor + 8) as usize;
        let ds_type = u32_at(data, cursor + 12);

        if ds_type == 1 {
            let list_start = cursor + ds_hs;
            if list_start + 12 <= data.len() && &data[list_start..list_start + 4] == b"mhlt" {
                let list_hs = u32_at(data, list_start + 4) as usize;
                let count = u32_at(data, list_start + 8);

                let mut pos = list_start + list_hs;
                for _ in 0..count {
                    if pos + 16 > data.len() || &data[pos..pos + 4] != b"mhit" {
                        break;
                    }
                    let mhit_hs = u32_at(data, pos + 4) as usize;
                    let mhit_total = u32_at(data, pos + 8) as usize;
                    let track_id = u32_at(data, pos + 16);
                    let dbid = if mhit_hs >= 120 {
                        u64_at(data, pos + 112)
                    } else {
                        0
                    };

                    let header_end = pos + mhit_hs.min(data.len() - pos);
                    results.push((track_id, dbid, data[pos..header_end].to_vec()));

                    pos += mhit_total;
                }
            }
        }

        cursor += ds_total;
    }

    results
}

/// Analyze extended region fields across all tracks.
fn analyze_extended_fields(headers: &[(u32, u64, Vec<u8>)]) -> BTreeMap<usize, FieldStats> {
    let mut stats: BTreeMap<usize, FieldStats> = BTreeMap::new();

    for (_tid, _dbid, header) in headers {
        let hs = header.len();
        // Scan every 4-byte aligned offset from 0 to header_size.
        let mut off = 0;
        while off + 4 <= hs {
            let val = u32::from_le_bytes(header[off..off + 4].try_into().unwrap());
            let entry = stats.entry(off).or_insert_with(|| FieldStats {
                values: Vec::new(),
                nonzero_count: 0,
                total_count: 0,
            });
            entry.total_count += 1;
            if val != 0 {
                entry.nonzero_count += 1;
            }
            entry.values.push(val);
            off += 4;
        }
    }

    stats
}

struct FieldStats {
    values: Vec<u32>,
    nonzero_count: usize,
    total_count: usize,
}

impl FieldStats {
    fn min_val(&self) -> u32 {
        self.values.iter().copied().min().unwrap_or(0)
    }
    fn max_val(&self) -> u32 {
        self.values.iter().copied().max().unwrap_or(0)
    }
    fn is_constant(&self) -> bool {
        self.values.windows(2).all(|w| w[0] == w[1])
    }
    fn unique_count(&self) -> usize {
        let mut v = self.values.clone();
        v.sort();
        v.dedup();
        v.len()
    }
}

fn print_analysis(label: &str, headers: &[(u32, u64, Vec<u8>)]) {
    println!("=== {} ({} tracks) ===\n", label, headers.len());

    if headers.is_empty() {
        println!("  (no tracks)\n");
        return;
    }

    let header_size = headers[0].2.len();
    println!("  mhit header_size: {} bytes\n", header_size);

    let stats = analyze_extended_fields(headers);

    // Print ALL fields, highlighting non-zero ones
    println!(
        "  {:>6}  {:>24}  {:>5}  {:>10}  {:>10}  {:>8}  NOTE",
        "OFFSET", "FIELD", "NZ/%", "MIN", "MAX", "UNIQUE"
    );
    println!("  {}", "-".repeat(90));

    for (&off, st) in &stats {
        let nz_pct = (st.nonzero_count as f64 / st.total_count as f64 * 100.0) as u32;

        // Only print fields that are non-zero on at least one track,
        // or are in the extended region where we need coverage.
        if st.nonzero_count == 0 && off < 212 {
            continue; // Skip known-zero core fields to reduce noise
        }
        if st.nonzero_count == 0 && off >= 212 {
            continue; // Skip always-zero extended fields
        }

        let note = if st.is_constant() && st.nonzero_count > 0 {
            format!("CONSTANT = 0x{:08x} ({})", st.values[0], st.values[0])
        } else if st.nonzero_count == st.total_count {
            "always set".to_string()
        } else if st.nonzero_count > 0 {
            format!("partial ({}/{})", st.nonzero_count, st.total_count)
        } else {
            String::new()
        };

        println!(
            "  +{:<5}  {:>24}  {:>4}%  0x{:08x}  0x{:08x}  {:>8}  {}",
            off,
            field_name(off),
            nz_pct,
            st.min_val(),
            st.max_val(),
            st.unique_count(),
            note
        );
    }

    // Print extended region summary (only non-zero fields)
    println!(
        "\n  --- Extended region (+212..+{}) non-zero fields ---\n",
        header_size
    );
    println!(
        "  {:>6}  {:>24}  {:>5}  {:>12}  {:>12}  CLASSIFICATION",
        "OFFSET", "FIELD", "NZ/%", "EXAMPLE_VAL", "HEX"
    );
    println!("  {}", "-".repeat(85));

    for (&off, st) in &stats {
        if off < 212 || st.nonzero_count == 0 {
            continue;
        }

        let nz_pct = (st.nonzero_count as f64 / st.total_count as f64 * 100.0) as u32;
        let example = st.values.iter().find(|&&v| v != 0).copied().unwrap_or(0);

        let classification = if st.is_constant() {
            "CONSTANT — hardcode this value"
        } else if st.nonzero_count == st.total_count && st.unique_count() == st.total_count {
            "PER-TRACK UNIQUE — needs generation"
        } else if st.nonzero_count == st.total_count {
            "ALWAYS SET — may vary by track metadata"
        } else {
            "OPTIONAL — sometimes set"
        };

        println!(
            "  +{:<5}  {:>24}  {:>4}%  {:>12}  0x{:08x}  {}",
            off,
            field_name(off),
            nz_pct,
            example,
            example,
            classification
        );
    }
    println!();
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: mhit_field_dump <iTunesDB_path> [comparison_iTunesDB_path]");
        eprintln!();
        eprintln!("Dumps every non-zero field in mhit headers to identify what the");
        eprintln!("from-scratch writer needs to populate for iPod Classic firmware.");
        std::process::exit(1);
    }

    let path1 = PathBuf::from(&args[1]);
    let data1 = std::fs::read(&path1).expect("Failed to read first iTunesDB");
    let headers1 = extract_mhit_headers(&data1);
    print_analysis(&format!("Reference: {}", path1.display()), &headers1);

    if args.len() >= 3 {
        let path2 = PathBuf::from(&args[2]);
        let data2 = std::fs::read(&path2).expect("Failed to read second iTunesDB");
        let headers2 = extract_mhit_headers(&data2);
        print_analysis(&format!("Ours: {}", path2.display()), &headers2);

        // Diff: show fields where reference has non-zero but ours doesn't
        println!("=== DIFF: Fields in reference but MISSING in ours ===\n");
        let stats1 = analyze_extended_fields(&headers1);
        let stats2 = analyze_extended_fields(&headers2);

        println!(
            "  {:>6}  {:>24}  {:>12}  {:>12}  STATUS",
            "OFFSET", "FIELD", "REF_EXAMPLE", "OURS"
        );
        println!("  {}", "-".repeat(75));

        for (&off, st1) in &stats1 {
            if off < 212 || st1.nonzero_count == 0 {
                continue;
            }
            let example1 = st1.values.iter().find(|&&v| v != 0).copied().unwrap_or(0);

            let st2 = stats2.get(&off);
            let ours_nz = st2.map(|s| s.nonzero_count).unwrap_or(0);
            let ours_example = st2
                .and_then(|s| s.values.iter().find(|&&v| v != 0).copied())
                .unwrap_or(0);

            let status = if ours_nz == 0 {
                "*** MISSING ***"
            } else if st1.is_constant()
                && st2.map(|s| s.is_constant()).unwrap_or(false)
                && example1 == ours_example
            {
                "OK (match)"
            } else if ours_nz > 0 {
                "OK (set)"
            } else {
                "???"
            };

            if ours_nz == 0 || status.contains("MISSING") {
                println!(
                    "  +{:<5}  {:>24}  0x{:08x}  0x{:08x}  {}",
                    off,
                    field_name(off),
                    example1,
                    ours_example,
                    status
                );
            }
        }
        println!();
    }

    // Also dump the first track's core fields for sanity checking
    if let Some((tid, dbid, header)) = headers1.first() {
        println!(
            "=== First track core fields (track_id={}, dbid=0x{:016x}) ===\n",
            tid, dbid
        );
        let core_offsets = [
            32, 36, 40, 44, 48, 52, 56, 60, 80, 84, 88, 92, 96, 100, 104, 108, 132, 156, 192, 196,
            200, 204, 208,
        ];
        for &off in &core_offsets {
            if off + 4 <= header.len() {
                let val = u32::from_le_bytes(header[off..off + 4].try_into().unwrap());
                if val != 0 {
                    println!(
                        "  +{:<5}  {:>24}  = {} (0x{:08x})",
                        off,
                        field_name(off),
                        val,
                        val
                    );
                }
            }
        }
    }
}
