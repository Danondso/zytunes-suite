use zune_mtp::{MtpSession, MtpzKeys};

fn main() {
    let log = |msg: &str| eprintln!("  {msg}");

    eprintln!("=== IOKit MTP Test ===");
    eprintln!("Opening USB device (vid=0x045e, pid=0x0710)...");
    let mut session = match MtpSession::open(0x045e, 0x0710) {
        Ok(s) => {
            eprintln!("OK: MTP session opened");
            s
        }
        Err(e) => {
            eprintln!("FAIL: {e}");
            return;
        }
    };

    eprintln!("Loading MTPZ keys from ~/.mtpz-data...");
    let keys = match MtpzKeys::load_default() {
        Ok(k) => {
            eprintln!("OK: Keys loaded");
            k
        }
        Err(e) => {
            eprintln!("FAIL: {e}");
            return;
        }
    };

    eprintln!("Starting MTPZ handshake...");
    match zune_mtp::mtpz::authenticate(&mut session, &keys, &log) {
        Ok(()) => eprintln!("OK: MTPZ authenticated!"),
        Err(e) => {
            eprintln!("FAIL: {e}");
            return;
        }
    }

    eprintln!("Getting storage IDs...");
    match session.get_storage_ids() {
        Ok(ids) => eprintln!("OK: Storage IDs: {:?}", ids),
        Err(e) => eprintln!("FAIL: {e}"),
    }

    eprintln!("=== Done ===");
}
