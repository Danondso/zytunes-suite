use zune_mtp::{MtpSession, MtpzKeys};

fn main() {
    let log = |msg: &str| eprintln!("  {msg}");

    let mut session = MtpSession::open(0x045e, 0x0710).unwrap();
    let keys = MtpzKeys::load_default().unwrap();
    zune_mtp::mtpz::authenticate(&mut session, &keys, &log).unwrap();

    let storage_ids = session.get_storage_ids().unwrap();
    let sid = storage_ids[0];
    eprintln!("Storage: {sid}");

    // List root
    let root_handles = session.get_object_handles(sid, 0xFFFFFFFF).unwrap();
    eprintln!("Root: {} objects", root_handles.len());
    for h in &root_handles {
        if let Ok(info) = session.get_object_info(*h) {
            eprintln!("  {} (0x{:04x}) {}", info.filename, info.object_format, info.compressed_size);
        }
    }

    // Check if Music exists
    for h in &root_handles {
        if let Ok(info) = session.get_object_info(*h) {
            if info.filename == "Music" {
                let music_handles = session.get_object_handles(sid, *h).unwrap();
                eprintln!("\nMusic: {} artists", music_handles.len());
                for ah in music_handles.iter().take(5) {
                    if let Ok(ainfo) = session.get_object_info(*ah) {
                        let album_handles = session.get_object_handles(sid, *ah).unwrap();
                        eprintln!("  {} ({} albums)", ainfo.filename, album_handles.len());
                    }
                }
                if music_handles.len() > 5 {
                    eprintln!("  ... and {} more", music_handles.len() - 5);
                }
            }
        }
    }
}
