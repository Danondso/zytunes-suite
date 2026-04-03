use std::time::Instant;
use zune_mtp::{MtpSession, MtpzKeys};

fn main() {
    let log = |msg: &str| eprintln!("  {msg}");

    eprintln!("Opening session...");
    let mut session = MtpSession::open(0x045e, 0x0710).unwrap();
    let keys = MtpzKeys::load_default().unwrap();
    zune_mtp::mtpz::authenticate(&mut session, &keys, &log).unwrap();
    eprintln!("Authenticated.");

    let sid = session.get_storage_ids().unwrap()[0];

    // Find Music folder.
    let root = session.get_object_handles(sid, 0xFFFFFFFF).unwrap();
    let music_handle = root.iter().find_map(|h| {
        session.get_object_info(*h).ok().and_then(|info| {
            if info.filename == "Music" {
                Some(*h)
            } else {
                None
            }
        })
    });

    let music = match music_handle {
        Some(h) => h,
        None => {
            eprintln!("No Music folder found");
            return;
        }
    };

    eprintln!("Found Music folder (handle {}), listing tracks...", music);
    let start = Instant::now();

    // Walk the tree: Music -> Artist -> Album -> Tracks
    let artists = session.get_object_handles(sid, music).unwrap();
    eprintln!("{} artists", artists.len());

    let mut total_tracks = 0;
    for artist_handle in &artists {
        let artist_info = match session.get_object_info(*artist_handle) {
            Ok(i) => i,
            Err(_) => continue,
        };
        let albums = session
            .get_object_handles(sid, *artist_handle)
            .unwrap_or_default();
        for album_handle in &albums {
            let album_info = match session.get_object_info(*album_handle) {
                Ok(i) => i,
                Err(_) => continue,
            };
            let tracks = session
                .get_object_handles(sid, *album_handle)
                .unwrap_or_default();
            for track_handle in &tracks {
                let track_info = match session.get_object_info(*track_handle) {
                    Ok(i) => i,
                    Err(_) => continue,
                };
                if track_info.object_format != 0x3001 {
                    total_tracks += 1;
                    if total_tracks <= 3 {
                        eprintln!(
                            "  {}/{}/{} ({})",
                            artist_info.filename,
                            album_info.filename,
                            track_info.filename,
                            track_info.compressed_size
                        );
                    }
                }
            }
        }
        if total_tracks > 3 && artist_handle == artists.last().unwrap() {
            eprintln!("  ...");
        }
    }

    let elapsed = start.elapsed();
    eprintln!(
        "\n{} tracks found in {:.1}s",
        total_tracks,
        elapsed.as_secs_f64()
    );
}
