//! Dump every MTP property the device has stored for a specific track object.
//!
//! Navigates `/Music/{artist}/{album}/{filename}` and calls
//! `GetObjectPropList (0x9805)` against that handle with property=0xFFFFFFFF
//! to retrieve every property the device knows about for that object.
//!
//! Used to diagnose "Unknown Artist" — does the device have our Artist string
//! committed, or was it dropped silently during sync?
//!
//! ```
//! ZUNE_PID=063e ARTIST='*NSYNC' ALBUM='No Strings Attached' \
//!   TRACK='01 Bye Bye Bye.mp3' \
//!   cargo run -p zune-mtp --example probe_artist
//! ```

use zune_mtp::{MtpSession, MtpzKeys};

fn prop_label(code: u16) -> &'static str {
    match code {
        0xDC01 => "StorageID",
        0xDC02 => "ObjectFormat",
        0xDC03 => "ProtectionStatus",
        0xDC04 => "ObjectSize",
        0xDC07 => "ObjectFileName",
        0xDC09 => "DateModified",
        0xDC0B => "ParentObject",
        0xDC41 => "PersistentUID",
        0xDC44 => "Name",
        0xDC46 => "Artist",
        0xDAB9 => "ArtistID (MTP ext)",
        0xDC47 => "Date Authored",
        0xDC48 => "Composer",
        0xDC4A => "Date",
        0xDC4B => "Genre",
        0xDC4C => "Duration",
        0xDC4D => "Rating",
        0xDC4E => "Track",
        0xDC86 => "RepresentativeSampleData",
        0xDC89 => "SampleRate",
        0xDC8B => "AudioBitDepth",
        0xDC8C => "AudioBitRate",
        0xDC9A => "AlbumName",
        0xDC9B => "AlbumArtist",
        _ => "?",
    }
}

fn datatype_label(dt: u16) -> &'static str {
    match dt {
        0x0000 => "Undefined",
        0x0001 => "INT8",
        0x0002 => "UINT8",
        0x0003 => "INT16",
        0x0004 => "UINT16",
        0x0005 => "INT32",
        0x0006 => "UINT32",
        0x0007 => "INT64",
        0x0008 => "UINT64",
        0xFFFF => "STRING",
        _ => "?",
    }
}

fn find_child(session: &mut MtpSession, storage: u32, parent: u32, name: &str) -> Option<u32> {
    let handles = session.get_object_handles(storage, parent).ok()?;
    for h in handles {
        let info = session.get_object_info(h).ok()?;
        if info.filename.eq_ignore_ascii_case(name) {
            return Some(h);
        }
    }
    None
}

fn parse_mtp_string(data: &[u8], offset: &mut usize) -> Option<String> {
    if *offset >= data.len() {
        return None;
    }
    let num_chars = data[*offset] as usize;
    *offset += 1;
    if num_chars == 0 {
        return Some(String::new());
    }
    let byte_len = num_chars * 2;
    if *offset + byte_len > data.len() {
        return None;
    }
    let chars: Vec<u16> = data[*offset..*offset + byte_len]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| u16::from_le_bytes(*b))
        .filter(|&c| c != 0)
        .collect();
    *offset += byte_len;
    String::from_utf16(&chars).ok()
}

fn main() {
    let log = |msg: &str| eprintln!("  {msg}");

    let pid = std::env::var("ZUNE_PID")
        .ok()
        .and_then(|s| u16::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .unwrap_or(0x0710);
    let artist = std::env::var("ARTIST").unwrap_or_else(|_| "*NSYNC".to_string());
    let album = std::env::var("ALBUM").unwrap_or_else(|_| "No Strings Attached".to_string());
    let track = std::env::var("TRACK").unwrap_or_else(|_| "01 Bye Bye Bye.mp3".to_string());

    print!("Opening USB session (pid=0x{pid:04x})... ");
    let mut session = match MtpSession::open(0x045e, pid) {
        Ok(s) => {
            println!("ok");
            s
        }
        Err(e) => {
            println!("FAILED: {e}");
            return;
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
            return;
        }
    };

    print!("MTPZ handshake... ");
    if let Err(e) = zune_mtp::mtpz::authenticate(&mut session, &keys, &log) {
        println!("FAILED: {e}");
        return;
    }
    println!("ok");

    let storage = match session.get_storage_ids() {
        Ok(ids) if !ids.is_empty() => ids[0],
        _ => {
            println!("no storage");
            return;
        }
    };

    const MTP_ROOT: u32 = 0xFFFFFFFF;
    let music = find_child(&mut session, storage, MTP_ROOT, "Music").expect("no /Music");
    let art = find_child(&mut session, storage, music, &artist).expect("artist folder not found");
    let alb = find_child(&mut session, storage, art, &album).expect("album folder not found");
    let trk = find_child(&mut session, storage, alb, &track).expect("track not found");

    println!();
    println!("Track handle: {trk} (0x{trk:08x})");
    println!("Path: /Music/{artist}/{album}/{track}");
    println!();

    // Targeted query: just PROP_ARTIST (0xDC46) and PROP_ARTIST_ID (0xDAB9)
    // to show what the HD actually has stored for this track's artist.
    for (name, prop) in [
        ("Artist (0xDC46)", 0xDC46u32),
        ("ArtistID (0xDAB9)", 0xDAB9u32),
    ] {
        match session.get_object_prop_list(trk, 0, prop, 0, 0) {
            Ok(d) => {
                println!("--- {name} ---");
                println!("  raw {} bytes: {:02x?}", d.len(), d);
                if d.len() >= 12 && prop == 0xDAB9 {
                    // count(4) + handle(4) + prop(2) + type(2) + value(4)
                    let artist_id = u32::from_le_bytes([d[12], d[13], d[14], d[15]]);
                    if artist_id != 0 {
                        println!("  => querying artist object 0x{artist_id:08x}...");
                        match session.get_object_info(artist_id) {
                            Ok(info) => {
                                println!(
                                    "  artist object: format=0x{:04x} filename={:?}",
                                    info.object_format, info.filename
                                );
                            }
                            Err(e) => println!("  (get_object_info failed: {e})"),
                        }
                    }
                }
            }
            Err(e) => println!("--- {name}: {e} ---"),
        }
        println!();
    }

    // Also dump the full prop list for reference.
    let data = match session.get_object_prop_list(trk, 0, 0xFFFFFFFF, 0, 0) {
        Ok(d) => d,
        Err(e) => {
            println!("GetObjectPropList failed: {e}");
            return;
        }
    };

    if data.len() < 4 {
        println!("Response too short: {} bytes", data.len());
        return;
    }
    let count = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    println!("--- {count} properties stored on device ---");

    let mut off = 4usize;
    for _ in 0..count {
        if off + 8 > data.len() {
            println!("(truncated)");
            return;
        }
        let _obj = u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]);
        let prop = u16::from_le_bytes([data[off + 4], data[off + 5]]);
        let dtype = u16::from_le_bytes([data[off + 6], data[off + 7]]);
        off += 8;

        let label = prop_label(prop);
        let dlabel = datatype_label(dtype);
        print!("  0x{prop:04x} {label:<30} [{dlabel}] = ");

        match dtype {
            0x0004 => {
                if off + 2 <= data.len() {
                    let v = u16::from_le_bytes([data[off], data[off + 1]]);
                    println!("{v}");
                    off += 2;
                }
            }
            0x0006 => {
                if off + 4 <= data.len() {
                    let v = u32::from_le_bytes([
                        data[off],
                        data[off + 1],
                        data[off + 2],
                        data[off + 3],
                    ]);
                    println!("0x{v:08x} ({v})");
                    off += 4;
                }
            }
            0x0008 => {
                if off + 8 <= data.len() {
                    let v = u64::from_le_bytes([
                        data[off],
                        data[off + 1],
                        data[off + 2],
                        data[off + 3],
                        data[off + 4],
                        data[off + 5],
                        data[off + 6],
                        data[off + 7],
                    ]);
                    println!("{v}");
                    off += 8;
                }
            }
            0xFFFF => match parse_mtp_string(&data, &mut off) {
                Some(s) if s.is_empty() => println!("\"\" (empty)"),
                Some(s) => println!("\"{s}\""),
                None => {
                    println!("(string parse failed)");
                    return;
                }
            },
            _ => {
                println!("(skipping unknown type — can't know value length)");
                return;
            }
        }
    }
}
