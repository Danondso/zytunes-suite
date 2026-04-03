use zune_mtp::proplist::*;
use zune_mtp::{MtpSession, MtpzKeys};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: test_import <file.mp3>");
        return;
    }
    let path = &args[1];

    let log = |msg: &str| eprintln!("  {msg}");

    eprintln!("Opening session...");
    let mut session = MtpSession::open(0x045e, 0x0710).unwrap();
    let keys = MtpzKeys::load_default().unwrap();
    zune_mtp::mtpz::authenticate(&mut session, &keys, &log).unwrap();

    let sid = session.get_storage_ids().unwrap()[0];
    eprintln!("Storage: {sid}");

    // Read file.
    let file_data = std::fs::read(path).unwrap();
    let filename = std::path::Path::new(path)
        .file_name()
        .unwrap()
        .to_str()
        .unwrap();

    eprintln!("File: {} ({} bytes)", filename, file_data.len());

    // Find Music folder.
    let root = session.get_object_handles(sid, 0xFFFFFFFF).unwrap();
    let music = root
        .iter()
        .find_map(|h| {
            session.get_object_info(*h).ok().and_then(|info| {
                if info.filename == "Music" {
                    Some(*h)
                } else {
                    None
                }
            })
        })
        .expect("No Music folder");

    eprintln!("Music folder: {music}");

    // Try creating a track via SendObjectPropList.
    let props = PropListBuilder::new()
        .add_string(PROP_NAME, "Test Track")
        .add_string(PROP_OBJECT_FILENAME, filename)
        .build();

    eprintln!("Sending ObjectPropList ({} bytes)...", props.len());
    match session.send_object_prop_list(sid, music, 0x3009, file_data.len() as u64, &props) {
        Ok((s, p, id)) => {
            eprintln!("OK: storage={s} parent={p} object_id={id}");
            eprintln!("Uploading {} bytes...", file_data.len());
            match session.send_object(&file_data) {
                Ok(()) => eprintln!("Upload OK!"),
                Err(e) => eprintln!("Upload failed: {e}"),
            }
        }
        Err(e) => eprintln!("SendObjectPropList failed: {e}"),
    }
}
