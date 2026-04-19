//! Probe whether firmware on the attached Zune accepts the MTP standard bulk
//! property-list ops. Intended for older firmware (1.x) that predates ZMDB —
//! we want to know whether `GetObjectPropList (0x9805)` and
//! `GetObjectPropsSupported (0x9806)` work, since those are the standard
//! (non-vendor) equivalents.
//!
//! Run against a connected Zune:
//!
//! ```
//! cargo run -p zune-mtp --example probe_bulk_query
//! ```
//!
//! By default it targets a Zune 30 (pid=0x0710). Set `ZUNE_PID` in hex to
//! target a different model — e.g. `ZUNE_PID=063e` for a Zune HD.
//!
//! No writes are made. Paste the output back when asking for help.

use std::time::Instant;

use zune_mtp::{MtpError, MtpSession, MtpzKeys};

/// Common MTP audio object-format codes.
const FORMAT_UNDEFINED: u16 = 0x3000;
const FORMAT_MP3: u16 = 0x3009;
const FORMAT_WMA: u16 = 0xB901;
const FORMAT_AAC: u16 = 0xB903;

/// A short human label for each property code we're likely to see in the dump.
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
        0xDC48 => "Composer",
        0xDC4A => "Date",
        0xDC4B => "Genre",
        0xDC4C => "Duration",
        0xDC4D => "Rating",
        0xDC4E => "Track",
        0xDC89 => "SampleRate",
        0xDC8B => "AudioBitDepth",
        0xDC9A => "AlbumName",
        0xDC9B => "AlbumArtist",
        0xDE93 => "AudioWAVECodec",
        0xDE94 => "AudioBitRate",
        _ => "?",
    }
}

fn main() {
    let log = |msg: &str| eprintln!("  {msg}");

    println!("=== Zune MTP bulk-query probe ===");
    println!();
    let pid = std::env::var("ZUNE_PID")
        .ok()
        .and_then(|s| u16::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .unwrap_or(0x0710);
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

    println!();
    if let Ok(v) = session.get_device_version() {
        println!("Firmware version: {v}");
    } else if let Ok(v) = session.get_device_prop_string(0xD404) {
        println!("Firmware version (D404): {v}");
    }
    println!();

    // 1. Ask the device which properties it exposes for each audio format.
    println!("--- GetObjectPropsSupported (0x9806) per format ---");
    for (label, fmt) in [
        ("Undefined", FORMAT_UNDEFINED),
        ("MP3", FORMAT_MP3),
        ("WMA", FORMAT_WMA),
        ("AAC", FORMAT_AAC),
    ] {
        match session.get_object_props_supported(fmt) {
            Ok(props) => {
                println!("  {label} (0x{fmt:04x}): {} props", props.len());
                for p in &props {
                    println!("    0x{p:04x}  {}", prop_label(*p));
                }
            }
            Err(e) => println!("  {label} (0x{fmt:04x}): {}", classify(&e)),
        }
    }
    println!();

    // 2. Probe GetObjectPropList — the standard bulk query. Devices that
    //    support it return the whole library's metadata in one response.
    //    object_id=0xFFFFFFFF + depth=0xFFFFFFFF means "everything under root".
    //    property=0xFFFFFFFF means "all supported props".
    println!("--- GetObjectPropList (0x9805) ---");
    for (label, object_id, format, property, group, depth) in [
        (
            "all MP3, all props, depth=all",
            0xFFFFFFFFu32,
            FORMAT_MP3 as u32,
            0xFFFFFFFFu32,
            0u32,
            0xFFFFFFFFu32,
        ),
        (
            "all WMA, all props, depth=all",
            0xFFFFFFFFu32,
            FORMAT_WMA as u32,
            0xFFFFFFFFu32,
            0u32,
            0xFFFFFFFFu32,
        ),
        (
            "all formats, all props, depth=all",
            0xFFFFFFFFu32,
            0u32,
            0xFFFFFFFFu32,
            0u32,
            0xFFFFFFFFu32,
        ),
        (
            "all MP3, ObjectFileName only, depth=all",
            0xFFFFFFFFu32,
            FORMAT_MP3 as u32,
            0xDC07u32,
            0u32,
            0xFFFFFFFFu32,
        ),
    ] {
        print!("  {label}: ");
        let start = Instant::now();
        match session.get_object_prop_list(object_id, format, property, group, depth) {
            Ok(data) => {
                let elapsed = start.elapsed();
                println!("{} bytes in {:?}", data.len(), elapsed);
                if data.len() >= 4 {
                    // Payload begins with u32 element count.
                    let count = u32::from_le_bytes(data[..4].try_into().unwrap());
                    println!("    reported element count: {count}");
                }
            }
            Err(e) => println!("{}", classify(&e)),
        }
    }
    println!();
    println!("Done. If GetObjectPropList succeeded above, firmware 1.x has a");
    println!("standard bulk-query path we can use as a ZMDB substitute.");
}

fn classify(e: &MtpError) -> String {
    if e.is_operation_not_supported() {
        "not supported (0x2005)".to_string()
    } else if let MtpError::DeviceRejected(code) = e {
        format!("device rejected (0x{code:04x})")
    } else {
        format!("error: {e}")
    }
}
