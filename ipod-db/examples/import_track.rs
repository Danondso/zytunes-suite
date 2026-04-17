//! Import a single audio file to the iPod: copy to F-dir, update iTunesDB.
//! Reads metadata via ffprobe (no lofty dependency needed in ipod-db).
//!
//! Usage:
//!   cargo run -p ipod-db --example import_track --release -- <mount> <fwid> <audio_file>

use std::path::PathBuf;
use std::process::Command;

fn ffprobe_field(path: &str, field: &str) -> Option<String> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "quiet",
            "-print_format",
            "default=noprint_wrappers=1:nokey=1",
            "-show_entries",
            &format!("format_tags={field}"),
            path,
        ])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() || s == "N/A" {
        None
    } else {
        Some(s)
    }
}

fn ffprobe_stream(path: &str, field: &str) -> Option<String> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "quiet",
            "-print_format",
            "default=noprint_wrappers=1:nokey=1",
            "-select_streams",
            "a:0",
            "-show_entries",
            &format!("stream={field}"),
            path,
        ])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() || s == "N/A" {
        None
    } else {
        Some(s)
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!("Usage: import_track <mount_path> <firewire_guid> <audio_file>");
        std::process::exit(1);
    }

    let mount = PathBuf::from(&args[1]);
    let fwid_str = &args[2];
    let audio_path = &args[3];

    let db_path = mount.join("iPod_Control/iTunes/iTunesDB");
    let raw = std::fs::read(&db_path).unwrap();
    let mut db = ipod_db::itunesdb::parse(&raw, mount.clone()).unwrap();
    println!("Existing: {} tracks", db.tracks.len());

    let file_size = std::fs::metadata(audio_path).unwrap().len() as u32;
    let ext = PathBuf::from(audio_path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("mp3")
        .to_lowercase();

    let title = ffprobe_field(audio_path, "title").unwrap_or_else(|| {
        PathBuf::from(audio_path)
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into()
    });
    let artist = ffprobe_field(audio_path, "artist").unwrap_or_else(|| "Unknown Artist".into());
    let album = ffprobe_field(audio_path, "album").unwrap_or_else(|| "Unknown Album".into());
    let genre = ffprobe_field(audio_path, "genre");
    let track_raw = ffprobe_field(audio_path, "track");
    let track_number = track_raw
        .as_ref()
        .and_then(|s| s.split('/').next().and_then(|n| n.parse::<u16>().ok()));
    let total_tracks = track_raw
        .as_ref()
        .and_then(|s| s.split('/').nth(1).and_then(|n| n.parse::<u16>().ok()));
    let disc_raw = ffprobe_field(audio_path, "disc");
    let disc_number = disc_raw
        .as_ref()
        .and_then(|s| s.split('/').next().and_then(|n| n.parse::<u16>().ok()));
    let total_discs = disc_raw
        .as_ref()
        .and_then(|s| s.split('/').nth(1).and_then(|n| n.parse::<u16>().ok()));
    let year =
        ffprobe_field(audio_path, "date").and_then(|s| s[..4.min(s.len())].parse::<u16>().ok());

    let codec = ffprobe_stream(audio_path, "codec_name").unwrap_or_default();
    let bitrate_str = ffprobe_stream(audio_path, "bit_rate");
    let bitrate = bitrate_str
        .as_ref()
        .and_then(|s| s.parse::<u64>().ok())
        .map(|b| (b / 1000) as u16);
    let sample_rate = ffprobe_stream(audio_path, "sample_rate").and_then(|s| s.parse::<u16>().ok());
    let duration_str = Command::new("ffprobe")
        .args([
            "-v",
            "quiet",
            "-print_format",
            "default=noprint_wrappers=1:nokey=1",
            "-show_entries",
            "format=duration",
            audio_path,
        ])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    let duration_ms = duration_str
        .and_then(|s| s.parse::<f64>().ok())
        .map(|d| (d * 1000.0) as u32);

    let filetype: u32 = match ext.as_str() {
        "mp3" => 0x4d503320,
        "m4a" | "aac" => 0x4d344120,
        "wav" => 0x57415620,
        _ => 0x4d503320,
    };
    let filetype_string = match codec.as_str() {
        "alac" => Some("Apple Lossless audio file".to_string()),
        "aac" => Some("AAC audio file".to_string()),
        "mp3" => Some("MPEG audio file".to_string()),
        _ => None,
    };

    println!("Title:    {title}");
    println!("Artist:   {artist}");
    println!("Album:    {album}");
    println!(
        "Codec:    {codec} -> {:?}",
        filetype_string.as_deref().unwrap_or("auto")
    );
    println!(
        "Duration: {}ms, {}kbps, {}Hz",
        duration_ms.unwrap_or(0),
        bitrate.unwrap_or(0),
        sample_rate.unwrap_or(0)
    );

    // Copy to iPod.
    ipod_db::fs::ensure_f_dirs(&mount).unwrap();
    let f_dir = ipod_db::fs::pick_f_dir(&mount).unwrap();
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let hash_name = ipod_db::fs::hash_filename(audio_path, seed);
    let ipod_path = ipod_db::fs::ipod_path(f_dir, &hash_name);
    let real_dest = ipod_db::fs::real_path(&mount, &ipod_path);
    std::fs::copy(audio_path, &real_dest).unwrap();
    println!("Copied to: {ipod_path}");

    let mut track = ipod_db::IpodTrack::default();
    track.title = title;
    track.artist = artist;
    track.album = album;
    track.genre = genre;
    track.track_number = track_number;
    track.total_tracks = total_tracks;
    track.disc_number = disc_number;
    track.total_discs = total_discs;
    track.total_time_ms = duration_ms;
    track.year = year;
    track.file_size = file_size;
    track.bitrate = bitrate;
    track.sample_rate = sample_rate;
    track.ipod_path = ipod_path;
    track.filetype = filetype;
    track.filetype_string = filetype_string;

    let dbid = db.add_track(track);
    println!("Added (dbid=0x{dbid:016x}), total: {}", db.tracks.len());

    // Reassign all track IDs sequentially starting from 52 (libgpod-style).
    // iPod Classic firmware may require dense track IDs.
    db.reassign_track_ids();
    println!(
        "Reassigned track IDs: 52..{}",
        52 + db.tracks.len() as u32 - 1
    );

    let mut output = ipod_db::itunesdb_write::serialize(&db);
    let fwid = ipod_db::hash::parse_firewire_id(fwid_str).unwrap();
    ipod_db::hash::sign_hash58(&mut output, &fwid).unwrap();

    let backup = db_path.with_extension("pre-import-bak");
    std::fs::copy(&db_path, &backup).unwrap();
    std::fs::write(&db_path, &output).unwrap();
    println!("Written {} bytes. Eject and test.", output.len());
}
