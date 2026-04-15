//! Byte-diff two ArtworkDB files.
//!
//! Usage:
//!   cargo run -p ipod-db --example compare_artworkdb -- <reference> <candidate>
//!
//! Walks both ArtworkDB files chunk-by-chunk and reports per-field deltas for
//! every mhii (matched by song dbid), mhni (matched by correlation_id within
//! an mhii), and mhif (matched by correlation_id). Used to isolate which
//! fields our writer sets differently from an iTunes-generated reference.
//!
//! Design notes:
//! - No dependencies beyond std. Raw little-endian reads are enough.
//! - Fields are documented with their offsets from the ipodlinux.org iTunesDB
//!   wiki so diffs can be cross-referenced.
//! - Records are shown only when they differ; identical fields are skipped
//!   so real deltas jump out.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("Usage: compare_artworkdb <reference> <candidate>");
        return ExitCode::from(2);
    }

    let ref_path = PathBuf::from(&args[1]);
    let cand_path = PathBuf::from(&args[2]);

    let ref_bytes = match std::fs::read(&ref_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("failed to read {}: {e}", ref_path.display());
            return ExitCode::from(1);
        }
    };
    let cand_bytes = match std::fs::read(&cand_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("failed to read {}: {e}", cand_path.display());
            return ExitCode::from(1);
        }
    };

    let reference = match parse(&ref_bytes) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("reference parse failed: {e}");
            return ExitCode::from(1);
        }
    };
    let candidate = match parse(&cand_bytes) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("candidate parse failed: {e}");
            return ExitCode::from(1);
        }
    };

    println!("== ArtworkDB compare ==");
    println!(
        "  reference: {} ({} bytes)",
        ref_path.display(),
        ref_bytes.len()
    );
    println!(
        "  candidate: {} ({} bytes)",
        cand_path.display(),
        cand_bytes.len()
    );
    println!();

    diff_mhfd(&reference, &candidate);
    println!();
    diff_mhii(&reference, &candidate);
    println!();
    diff_mhif(&reference, &candidate);

    ExitCode::SUCCESS
}

// ---------- parsed model ----------

#[derive(Default, Debug)]
struct ArtworkDb {
    // mhfd root header fields.
    mhfd_total: u32,
    mhfd_db_type: u32,
    mhfd_unknown_0x10: u32,
    mhfd_num_datasets: u32,
    mhfd_unknown_tail: Vec<u8>,

    // mhli header (inside mhsd type=1).
    mhli_count: u32,

    // mhlf header (inside mhsd type=2).
    mhlf_count: u32,

    // Per-track image records, keyed by song dbid.
    mhii: BTreeMap<u64, Mhii>,

    // Per-ithmb file records, keyed by correlation_id.
    mhif: BTreeMap<u32, Mhif>,
}

#[derive(Debug, Clone)]
struct Mhii {
    /// Raw 152-byte header as-written, for full hex diffing.
    header: Vec<u8>,
    /// Child mhni records (unwrapped from their mhod type=2 containers),
    /// keyed by correlation_id.
    mhni: BTreeMap<u32, Mhni>,
    /// Payload of the mhod type=6 child (typically an mhaf blob), if present.
    mhaf: Option<Vec<u8>>,
}

#[derive(Debug, Clone)]
struct Mhni {
    /// Raw 76-byte header.
    header: Vec<u8>,
    /// Child mhod filename string, decoded from UTF-16LE.
    filename: String,
}

#[derive(Debug, Clone)]
struct Mhif {
    /// Raw 124-byte header.
    header: Vec<u8>,
}

// ---------- parsing ----------

fn u32_at(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(buf[off..off + 4].try_into().unwrap())
}
fn u64_at(buf: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(buf[off..off + 8].try_into().unwrap())
}
fn u16_at(buf: &[u8], off: usize) -> u16 {
    u16::from_le_bytes(buf[off..off + 2].try_into().unwrap())
}

fn parse(data: &[u8]) -> Result<ArtworkDb, String> {
    if data.len() < 24 || &data[0..4] != b"mhfd" {
        return Err("not an ArtworkDB (expected 'mhfd' magic)".into());
    }
    let mhfd_header_size = u32_at(data, 4) as usize;
    let mhfd_total = u32_at(data, 8);
    let db_type = u32_at(data, 12);
    let unknown_0x10 = u32_at(data, 16);
    let num_datasets = u32_at(data, 20);
    let mhfd_tail = data[24..mhfd_header_size].to_vec();

    let mut db = ArtworkDb {
        mhfd_total,
        mhfd_db_type: db_type,
        mhfd_unknown_0x10: unknown_0x10,
        mhfd_num_datasets: num_datasets,
        mhfd_unknown_tail: mhfd_tail,
        ..Default::default()
    };

    // Walk datasets.
    let mut cursor = mhfd_header_size;
    for _ in 0..num_datasets {
        if cursor + 16 > data.len() || &data[cursor..cursor + 4] != b"mhsd" {
            return Err(format!("expected 'mhsd' at offset {cursor}"));
        }
        let mhsd_header_size = u32_at(data, cursor + 4) as usize;
        let mhsd_total = u32_at(data, cursor + 8) as usize;
        let mhsd_type = u32_at(data, cursor + 12);

        let inner_start = cursor + mhsd_header_size;
        let inner_end = cursor + mhsd_total;

        match mhsd_type {
            1 => parse_mhli(&data[inner_start..inner_end], &mut db)?,
            2 => {
                // mhla (album artwork list) — empty on Classic, skip.
            }
            3 => parse_mhlf(&data[inner_start..inner_end], &mut db)?,
            other => eprintln!("  warning: unknown mhsd type {other}, skipping"),
        }

        cursor = inner_end;
    }

    Ok(db)
}

fn parse_mhli(data: &[u8], db: &mut ArtworkDb) -> Result<(), String> {
    if data.len() < 12 || &data[0..4] != b"mhli" {
        return Err("expected 'mhli'".into());
    }
    let header_size = u32_at(data, 4) as usize;
    db.mhli_count = u32_at(data, 8);

    let mut cursor = header_size;
    while cursor < data.len() {
        if &data[cursor..cursor + 4] != b"mhii" {
            return Err(format!("expected 'mhii' at mhli offset {cursor}"));
        }
        let mhii_header_size = u32_at(data, cursor + 4) as usize;
        let mhii_total = u32_at(data, cursor + 8) as usize;
        let num_children = u32_at(data, cursor + 12);
        let dbid = u64_at(data, cursor + 20);

        let header = data[cursor..cursor + mhii_header_size].to_vec();
        let mut mhni_map = BTreeMap::new();
        let mut mhaf_payload: Option<Vec<u8>> = None;

        // Children are mhod wrappers: type=2 around mhni, type=6 around mhaf.
        let mut child_cur = cursor + mhii_header_size;
        for _ in 0..num_children {
            if &data[child_cur..child_cur + 4] != b"mhod" {
                return Err(format!("expected 'mhod' at mhii child offset {child_cur}"));
            }
            let mhod_hs = u32_at(data, child_cur + 4) as usize;
            let mhod_total = u32_at(data, child_cur + 8) as usize;
            let mhod_type = u32_at(data, child_cur + 12);
            let payload = &data[child_cur + mhod_hs..child_cur + mhod_total];

            match mhod_type {
                2 => {
                    // Payload is an mhni.
                    if payload.len() < 4 || &payload[0..4] != b"mhni" {
                        return Err(format!("mhod type=2 at {child_cur} did not wrap an mhni"));
                    }
                    let mhni_hs = u32_at(payload, 4) as usize;
                    let mhni_total = u32_at(payload, 8) as usize;
                    let corr_id = u32_at(payload, 16);
                    let mhni_header = payload[0..mhni_hs].to_vec();
                    let filename = if mhni_total > mhni_hs {
                        parse_mhod_string(&payload[mhni_hs..mhni_total]).unwrap_or_default()
                    } else {
                        String::new()
                    };
                    mhni_map.insert(
                        corr_id,
                        Mhni {
                            header: mhni_header,
                            filename,
                        },
                    );
                }
                6 => {
                    mhaf_payload = Some(payload.to_vec());
                }
                other => {
                    eprintln!("  warning: unknown mhod type {other} in mhii (dbid={dbid:#x})");
                }
            }

            child_cur += mhod_total;
        }

        db.mhii.insert(
            dbid,
            Mhii {
                header,
                mhni: mhni_map,
                mhaf: mhaf_payload,
            },
        );
        cursor += mhii_total;
    }

    Ok(())
}

fn parse_mhlf(data: &[u8], db: &mut ArtworkDb) -> Result<(), String> {
    if data.len() < 12 || &data[0..4] != b"mhlf" {
        return Err("expected 'mhlf'".into());
    }
    let header_size = u32_at(data, 4) as usize;
    db.mhlf_count = u32_at(data, 8);

    let mut cursor = header_size;
    while cursor < data.len() {
        if &data[cursor..cursor + 4] != b"mhif" {
            return Err(format!("expected 'mhif' at mhlf offset {cursor}"));
        }
        let mhif_header_size = u32_at(data, cursor + 4) as usize;
        let mhif_total = u32_at(data, cursor + 8) as usize;
        let corr_id = u32_at(data, cursor + 12);

        let header = data[cursor..cursor + mhif_header_size].to_vec();
        db.mhif.insert(corr_id, Mhif { header });
        cursor += mhif_total;
    }

    Ok(())
}

fn parse_mhod_string(data: &[u8]) -> Option<String> {
    if data.len() < 40 || &data[0..4] != b"mhod" {
        return None;
    }
    let mhod_header_size = u32_at(data, 4) as usize;
    // String sub-header at +mhod_header_size: [string_position, string_len, encoding, padding]
    if data.len() < mhod_header_size + 16 {
        return None;
    }
    let string_len = u32_at(data, mhod_header_size + 4) as usize;
    let encoding = u32_at(data, mhod_header_size + 8);
    let string_start = mhod_header_size + 16;
    if data.len() < string_start + string_len {
        return None;
    }
    let bytes = &data[string_start..string_start + string_len];
    if encoding == 1 {
        // UTF-16LE.
        let words: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16(&words).ok()
    } else {
        String::from_utf8(bytes.to_vec()).ok()
    }
}

// ---------- diffing ----------

fn diff_mhfd(a: &ArtworkDb, b: &ArtworkDb) {
    println!("[mhfd root]");
    diff_u32("  total_size         ", a.mhfd_total, b.mhfd_total);
    diff_u32("  db_type      (+0x0C)", a.mhfd_db_type, b.mhfd_db_type);
    diff_u32(
        "  unknown      (+0x10)",
        a.mhfd_unknown_0x10,
        b.mhfd_unknown_0x10,
    );
    diff_u32(
        "  num_datasets (+0x14)",
        a.mhfd_num_datasets,
        b.mhfd_num_datasets,
    );
    diff_bytes(
        "  unknown_tail (+0x18..)",
        &a.mhfd_unknown_tail,
        &b.mhfd_unknown_tail,
    );

    println!("[mhli]");
    diff_u32("  count              ", a.mhli_count, b.mhli_count);
    println!("[mhlf]");
    diff_u32("  count              ", a.mhlf_count, b.mhlf_count);
}

fn diff_mhii(a: &ArtworkDb, b: &ArtworkDb) {
    println!("[mhii records — keyed by song dbid]");
    let only_a: Vec<_> = a.mhii.keys().filter(|k| !b.mhii.contains_key(k)).collect();
    let only_b: Vec<_> = b.mhii.keys().filter(|k| !a.mhii.contains_key(k)).collect();
    println!("  reference-only dbids: {} ({:?})", only_a.len(), only_a);
    println!("  candidate-only dbids: {} ({:?})", only_b.len(), only_b);

    for (dbid, ma) in &a.mhii {
        let Some(mb) = b.mhii.get(dbid) else { continue };
        let mut any = false;
        let mut mhaf_line: Option<String> = None;

        match (ma.mhaf.as_deref(), mb.mhaf.as_deref()) {
            (Some(ra), Some(rb)) if ra != rb => {
                any = true;
                mhaf_line = Some(format!(
                    "    mhaf payload differs: ref={}B cand={}B",
                    ra.len(),
                    rb.len()
                ));
            }
            (Some(_), None) => {
                any = true;
                mhaf_line = Some("    mhaf present in ref, missing in candidate".into());
            }
            (None, Some(_)) => {
                any = true;
                mhaf_line = Some("    mhaf present in candidate, missing in ref".into());
            }
            _ => {}
        }

        // mhii fields per ipodlinux.org iTunesDB wiki.
        // +0x10 image_id, +0x14 song_id, +0x1C unknown, +0x20 rating,
        // +0x24 unknown, +0x28 original_date, +0x2C digitized_date,
        // +0x30 source_image_size.
        any |= diff_named_u32(
            "    image_id           (+0x10)",
            u32_at(&ma.header, 16),
            u32_at(&mb.header, 16),
        );
        any |= diff_named_u32(
            "    unknown            (+0x1C)",
            u32_at(&ma.header, 28),
            u32_at(&mb.header, 28),
        );
        any |= diff_named_u32(
            "    rating             (+0x20)",
            u32_at(&ma.header, 32),
            u32_at(&mb.header, 32),
        );
        any |= diff_named_u32(
            "    unknown            (+0x24)",
            u32_at(&ma.header, 36),
            u32_at(&mb.header, 36),
        );
        any |= diff_named_u32(
            "    original_date      (+0x28)",
            u32_at(&ma.header, 40),
            u32_at(&mb.header, 40),
        );
        any |= diff_named_u32(
            "    digitized_date     (+0x2C)",
            u32_at(&ma.header, 44),
            u32_at(&mb.header, 44),
        );
        any |= diff_named_u32(
            "    source_image_size  (+0x30)",
            u32_at(&ma.header, 48),
            u32_at(&mb.header, 48),
        );
        any |= diff_named_bytes(
            "    header_tail       (+0x34..)",
            &ma.header[52..],
            &mb.header[52..],
        );

        // Nested mhni diff.
        let corr_only_a: Vec<_> = ma
            .mhni
            .keys()
            .filter(|k| !mb.mhni.contains_key(k))
            .collect();
        let corr_only_b: Vec<_> = mb
            .mhni
            .keys()
            .filter(|k| !ma.mhni.contains_key(k))
            .collect();
        let mut mhni_any = false;
        let mut mhni_lines: Vec<String> = Vec::new();
        if !corr_only_a.is_empty() || !corr_only_b.is_empty() {
            mhni_any = true;
            mhni_lines.push(format!(
                "      mhni corr-id sets differ: ref-only {:?}, cand-only {:?}",
                corr_only_a, corr_only_b
            ));
        }
        for (corr, na) in &ma.mhni {
            let Some(nb) = mb.mhni.get(corr) else {
                continue;
            };
            let mut lines: Vec<String> = Vec::new();
            // +0x0C num_children, +0x10 corr_id, +0x14 image_offset, +0x18 image_size,
            // +0x1C vert_pad u16, +0x1E horiz_pad u16, +0x20 height u16, +0x22 width u16.
            push_diff_u32(
                &mut lines,
                "        num_children      (+0x0C)",
                u32_at(&na.header, 12),
                u32_at(&nb.header, 12),
            );
            push_diff_u32(
                &mut lines,
                "        image_offset      (+0x14)",
                u32_at(&na.header, 20),
                u32_at(&nb.header, 20),
            );
            push_diff_u32(
                &mut lines,
                "        image_size        (+0x18)",
                u32_at(&na.header, 24),
                u32_at(&nb.header, 24),
            );
            push_diff_u16(
                &mut lines,
                "        vertical_padding  (+0x1C)",
                u16_at(&na.header, 28),
                u16_at(&nb.header, 28),
            );
            push_diff_u16(
                &mut lines,
                "        horizontal_padding(+0x1E)",
                u16_at(&na.header, 30),
                u16_at(&nb.header, 30),
            );
            push_diff_u16(
                &mut lines,
                "        height            (+0x20)",
                u16_at(&na.header, 32),
                u16_at(&nb.header, 32),
            );
            push_diff_u16(
                &mut lines,
                "        width             (+0x22)",
                u16_at(&na.header, 34),
                u16_at(&nb.header, 34),
            );
            push_diff_bytes(
                &mut lines,
                "        header_tail       (+0x24..)",
                &na.header[36..],
                &nb.header[36..],
            );
            if na.filename != nb.filename {
                lines.push(format!(
                    "        filename: ref={:?}  cand={:?}",
                    na.filename, nb.filename
                ));
            }

            if !lines.is_empty() {
                mhni_any = true;
                mhni_lines.push(format!("      mhni corr_id={}:", corr));
                mhni_lines.extend(lines);
            }
        }
        if mhni_any {
            any = true;
        }

        if any {
            println!("  mhii dbid={}  ({} mhni children)", dbid, ma.mhni.len());
            if let Some(line) = mhaf_line {
                println!("{line}");
            }
            if mhni_any {
                println!("    nested mhni:");
                for line in &mhni_lines {
                    println!("{line}");
                }
            }
        }
    }
}

fn diff_mhif(a: &ArtworkDb, b: &ArtworkDb) {
    println!("[mhif records — keyed by correlation_id]");
    let only_a: Vec<_> = a.mhif.keys().filter(|k| !b.mhif.contains_key(k)).collect();
    let only_b: Vec<_> = b.mhif.keys().filter(|k| !a.mhif.contains_key(k)).collect();
    println!("  reference-only corr-ids: {:?}", only_a);
    println!("  candidate-only corr-ids: {:?}", only_b);

    for (corr, ma) in &a.mhif {
        let Some(mb) = b.mhif.get(corr) else { continue };
        // +0x10 image_size. Tail is model-specific (nano vs classic).
        let mut lines: Vec<String> = Vec::new();
        push_diff_u32(
            &mut lines,
            "    image_size        (+0x10)",
            u32_at(&ma.header, 16),
            u32_at(&mb.header, 16),
        );
        push_diff_bytes(
            &mut lines,
            "    header_tail       (+0x14..)",
            &ma.header[20..],
            &mb.header[20..],
        );
        if !lines.is_empty() {
            println!("  mhif corr_id={}", corr);
            for line in lines {
                println!("{line}");
            }
        }
    }
}

// ---------- small diff helpers ----------

fn diff_u32(label: &str, a: u32, b: u32) {
    if a == b {
        println!("{label}: {a} (match)");
    } else {
        println!(
            "{label}: ref={a} ({:#010x})  cand={b} ({:#010x})  DIFF",
            a, b
        );
    }
}

fn diff_named_u32(label: &str, a: u32, b: u32) -> bool {
    if a != b {
        println!(
            "{label}: ref={a} ({:#010x})  cand={b} ({:#010x})  DIFF",
            a, b
        );
        true
    } else {
        false
    }
}

fn push_diff_u32(out: &mut Vec<String>, label: &str, a: u32, b: u32) {
    if a != b {
        out.push(format!(
            "{label}: ref={a} ({:#010x})  cand={b} ({:#010x})  DIFF",
            a, b
        ));
    }
}

fn push_diff_u16(out: &mut Vec<String>, label: &str, a: u16, b: u16) {
    if a != b {
        out.push(format!(
            "{label}: ref={a} ({:#06x})  cand={b} ({:#06x})  DIFF",
            a, b
        ));
    }
}

fn diff_bytes(label: &str, a: &[u8], b: &[u8]) {
    if a == b {
        println!("{label}: {} bytes (match)", a.len());
    } else {
        println!("{label}: DIFF");
        print_byte_diff(a, b, 4);
    }
}

fn diff_named_bytes(label: &str, a: &[u8], b: &[u8]) -> bool {
    if a != b {
        println!("{label}: DIFF");
        print_byte_diff(a, b, 6);
        true
    } else {
        false
    }
}

fn push_diff_bytes(out: &mut Vec<String>, label: &str, a: &[u8], b: &[u8]) {
    if a != b {
        out.push(format!("{label}: DIFF"));
        for line in format_byte_diff(a, b) {
            out.push(format!("          {line}"));
        }
    }
}

fn print_byte_diff(a: &[u8], b: &[u8], indent: usize) {
    let prefix = " ".repeat(indent);
    for line in format_byte_diff(a, b) {
        println!("{prefix}{line}");
    }
}

/// Returns a per-16-byte line listing of the differing regions.
fn format_byte_diff(a: &[u8], b: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let max = a.len().max(b.len());
    for row in (0..max).step_by(16) {
        let slice_a = &a[row.min(a.len())..(row + 16).min(a.len())];
        let slice_b = &b[row.min(b.len())..(row + 16).min(b.len())];
        if slice_a == slice_b {
            continue;
        }
        out.push(format!("+{:04x}  ref {}", row, hex(slice_a)));
        out.push(format!("       cand {}", hex(slice_b)));
    }
    out
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && i % 4 == 0 {
            s.push(' ');
        }
        s.push_str(&format!("{:02x}", b));
    }
    s
}
