/// Binary structure comparison tool for iTunesDB files.
///
/// Walks chunk headers (mhbd, mhsd, mhlt, mhit, mhod, mhyp, mhip, mhlp, mhla)
/// and prints a structural summary useful for debugging rewrite mismatches.
use std::env;
use std::fs;
use std::io::{Cursor, Read, Seek, SeekFrom};

use byteorder::{LittleEndian, ReadBytesExt};

fn read_magic(cur: &mut Cursor<&[u8]>) -> Option<[u8; 4]> {
    let mut buf = [0u8; 4];
    cur.read_exact(&mut buf).ok()?;
    Some(buf)
}

fn magic_str(m: &[u8; 4]) -> String {
    String::from_utf8_lossy(m).into_owned()
}

fn mhod_type_name(t: u32) -> &'static str {
    match t {
        1 => "title",
        2 => "location",
        3 => "album",
        4 => "artist",
        5 => "genre",
        6 => "filetype",
        7 => "comment",
        8 => "composer",
        9 => "category",
        12 => "grouping",
        14 => "album_artist",
        15 => "sort_artist",
        16 => "sort_title",
        17 => "sort_album",
        18 => "sort_album_artist",
        19 => "sort_composer",
        20 => "sort_tv_show",
        21 => "description",
        22 => "podcast_enclosure_url",
        23 => "podcast_rss_url",
        24 => "chapter_data",
        25 => "subtitle",
        27 => "tv_show",
        28 => "tv_episode_number",
        50 => "smart_playlist_data",
        51 => "smart_playlist_rules",
        52 => "sort_index",
        53 => "library_playlist_index",
        100 => "column_info",
        200 => "album_album_name",
        201 => "album_album_artist",
        202 => "album_sort_artist",
        _ => "unknown",
    }
}

fn mhsd_type_name(t: u32) -> &'static str {
    match t {
        1 => "tracks",
        2 => "playlists",
        3 => "podcasts",
        4 => "albums",
        5 => "smart_playlists",
        _ => "unknown",
    }
}

struct MhodInfo {
    mhod_type: u32,
    total_size: u32,
    header_size: u32,
}

struct MhitInfo {
    header_size: u32,
    total_size: u32,
    num_mhods: u32,
    track_id: u32,
    dbid: u64,
    mhods: Vec<MhodInfo>,
}

struct MhipInfo {
    header_size: u32,
    total_size: u32,
    num_mhods: u32,
    track_id: u32,
}

struct MhypInfo {
    header_size: u32,
    total_size: u32,
    num_mhods: u32,
    num_mhips: u32,
    is_master: bool,
    playlist_name: Option<String>,
    mhods: Vec<MhodInfo>,
    mhips: Vec<MhipInfo>,
}

struct MhsdInfo {
    ds_type: u32,
    header_size: u32,
    total_size: u32,
    // Track dataset
    track_count: Option<u32>,
    tracks: Vec<MhitInfo>,
    // Playlist dataset
    playlist_count: Option<u32>,
    playlists: Vec<MhypInfo>,
    // Album dataset
    album_count: Option<u32>,
}

struct MhbdInfo {
    header_size: u32,
    total_size: u32,
    db_version: u32,
    num_datasets: u32,
    db_id: u64,
    hashing_scheme: u16,
    datasets: Vec<MhsdInfo>,
}

fn read_mhod_string(cur: &mut Cursor<&[u8]>, start: u64, header_size: u32) -> Option<String> {
    // String mhods have a 16-byte sub-header at offset 24
    if header_size < 40 {
        return None;
    }
    cur.seek(SeekFrom::Start(start + 24)).ok()?;
    let _string_position = cur.read_u32::<LittleEndian>().ok()?;
    let string_byte_len = cur.read_u32::<LittleEndian>().ok()?;
    let _encoding = cur.read_u32::<LittleEndian>().ok()?;
    let _padding = cur.read_u32::<LittleEndian>().ok()?;

    if string_byte_len > 10 * 1024 * 1024 || string_byte_len == 0 {
        return None;
    }
    let mut data = vec![0u8; string_byte_len as usize];
    cur.read_exact(&mut data).ok()?;
    let (decoded, _, _) = encoding_rs::UTF_16LE.decode(&data);
    Some(decoded.into_owned())
}

fn parse_mhod(cur: &mut Cursor<&[u8]>) -> Option<(MhodInfo, Option<String>)> {
    let start = cur.position();
    let magic = read_magic(cur)?;
    if &magic != b"mhod" {
        eprintln!(
            "  WARNING: expected mhod at offset {}, got {:?}",
            start,
            magic_str(&magic)
        );
        return None;
    }
    let header_size = cur.read_u32::<LittleEndian>().ok()?;
    let total_size = cur.read_u32::<LittleEndian>().ok()?;
    let mhod_type = cur.read_u32::<LittleEndian>().ok()?;

    // Try to read string for types 1-18
    let string_val = if mhod_type >= 1 && mhod_type <= 18 {
        read_mhod_string(cur, start, header_size)
    } else {
        None
    };

    cur.seek(SeekFrom::Start(start + total_size as u64)).ok()?;
    Some((
        MhodInfo {
            mhod_type,
            total_size,
            header_size,
        },
        string_val,
    ))
}

fn parse_mhit(cur: &mut Cursor<&[u8]>) -> Option<MhitInfo> {
    let start = cur.position();
    let magic = read_magic(cur)?;
    if &magic != b"mhit" {
        eprintln!(
            "  WARNING: expected mhit at offset {}, got {:?}",
            start,
            magic_str(&magic)
        );
        return None;
    }
    let header_size = cur.read_u32::<LittleEndian>().ok()?;
    let total_size = cur.read_u32::<LittleEndian>().ok()?;
    let num_mhods = cur.read_u32::<LittleEndian>().ok()?;
    let track_id = cur.read_u32::<LittleEndian>().ok()?;

    // Read dbid at offset +112 if header is large enough
    let dbid = if header_size >= 120 {
        cur.seek(SeekFrom::Start(start + 112)).ok()?;
        cur.read_u64::<LittleEndian>().ok().unwrap_or(0)
    } else {
        0
    };

    // Jump to end of header to read child mhods
    cur.seek(SeekFrom::Start(start + header_size as u64)).ok()?;

    let mut mhods = Vec::new();
    for _ in 0..num_mhods {
        if let Some((info, _)) = parse_mhod(cur) {
            mhods.push(info);
        } else {
            break;
        }
    }

    // Ensure we're at end of mhit
    cur.seek(SeekFrom::Start(start + total_size as u64)).ok()?;

    Some(MhitInfo {
        header_size,
        total_size,
        num_mhods,
        track_id,
        dbid,
        mhods,
    })
}

fn parse_mhip(cur: &mut Cursor<&[u8]>) -> Option<MhipInfo> {
    let start = cur.position();
    let magic = read_magic(cur)?;
    if &magic != b"mhip" {
        eprintln!(
            "  WARNING: expected mhip at offset {}, got {:?}",
            start,
            magic_str(&magic)
        );
        return None;
    }
    let header_size = cur.read_u32::<LittleEndian>().ok()?;
    let total_size = cur.read_u32::<LittleEndian>().ok()?;
    let num_mhods = cur.read_u32::<LittleEndian>().ok()?;
    let _podcast_grouping = cur.read_u32::<LittleEndian>().ok()?;
    let _group_id = cur.read_u32::<LittleEndian>().ok()?;
    let track_id = cur.read_u32::<LittleEndian>().ok()?;

    // Skip any child mhods
    cur.seek(SeekFrom::Start(start + header_size as u64)).ok()?;
    for _ in 0..num_mhods {
        if parse_mhod(cur).is_none() {
            break;
        }
    }

    cur.seek(SeekFrom::Start(start + total_size as u64)).ok()?;

    Some(MhipInfo {
        header_size,
        total_size,
        num_mhods,
        track_id,
    })
}

fn parse_mhyp(cur: &mut Cursor<&[u8]>) -> Option<MhypInfo> {
    let start = cur.position();
    let magic = read_magic(cur)?;
    if &magic != b"mhyp" {
        eprintln!(
            "  WARNING: expected mhyp at offset {}, got {:?}",
            start,
            magic_str(&magic)
        );
        return None;
    }
    let header_size = cur.read_u32::<LittleEndian>().ok()?;
    let total_size = cur.read_u32::<LittleEndian>().ok()?;
    let num_mhods = cur.read_u32::<LittleEndian>().ok()?;
    let num_mhips = cur.read_u32::<LittleEndian>().ok()?;
    let is_master = cur.read_u32::<LittleEndian>().ok()? != 0;

    cur.seek(SeekFrom::Start(start + header_size as u64)).ok()?;

    let mut mhods = Vec::new();
    let mut playlist_name = None;
    for _ in 0..num_mhods {
        if let Some((info, string_val)) = parse_mhod(cur) {
            if info.mhod_type == 1 {
                playlist_name = string_val;
            }
            mhods.push(info);
        } else {
            break;
        }
    }

    let mut mhips = Vec::new();
    for _ in 0..num_mhips {
        if let Some(info) = parse_mhip(cur) {
            mhips.push(info);
        } else {
            break;
        }
    }

    cur.seek(SeekFrom::Start(start + total_size as u64)).ok()?;

    Some(MhypInfo {
        header_size,
        total_size,
        num_mhods,
        num_mhips,
        is_master,
        playlist_name,
        mhods,
        mhips,
    })
}

fn parse_mhsd(cur: &mut Cursor<&[u8]>) -> Option<MhsdInfo> {
    let start = cur.position();
    let magic = read_magic(cur)?;
    if &magic != b"mhsd" {
        eprintln!(
            "  WARNING: expected mhsd at offset {}, got {:?}",
            start,
            magic_str(&magic)
        );
        return None;
    }
    let header_size = cur.read_u32::<LittleEndian>().ok()?;
    let total_size = cur.read_u32::<LittleEndian>().ok()?;
    let ds_type = cur.read_u32::<LittleEndian>().ok()?;

    cur.seek(SeekFrom::Start(start + header_size as u64)).ok()?;

    let mut info = MhsdInfo {
        ds_type,
        header_size,
        total_size,
        track_count: None,
        tracks: Vec::new(),
        playlist_count: None,
        playlists: Vec::new(),
        album_count: None,
    };

    // Parse the list header inside this dataset
    let list_start = cur.position();
    if let Some(list_magic) = read_magic(cur) {
        let list_header_size = cur.read_u32::<LittleEndian>().unwrap_or(0);
        let list_count = cur.read_u32::<LittleEndian>().unwrap_or(0);

        let list_tag = magic_str(&list_magic);
        println!(
            "    list: {} header_size={} count={}",
            list_tag, list_header_size, list_count
        );

        cur.seek(SeekFrom::Start(list_start + list_header_size as u64))
            .ok()?;

        match list_tag.as_str() {
            "mhlt" => {
                info.track_count = Some(list_count);
                for _ in 0..list_count {
                    if let Some(track) = parse_mhit(cur) {
                        info.tracks.push(track);
                    } else {
                        break;
                    }
                }
            }
            "mhlp" => {
                info.playlist_count = Some(list_count);
                for _ in 0..list_count {
                    if let Some(pl) = parse_mhyp(cur) {
                        info.playlists.push(pl);
                    } else {
                        break;
                    }
                }
            }
            "mhla" => {
                info.album_count = Some(list_count);
                // Album items — skip them, just count
            }
            _ => {}
        }
    }

    // Jump to end of mhsd
    cur.seek(SeekFrom::Start(start + total_size as u64)).ok()?;

    Some(info)
}

fn analyze_file(path: &str) {
    let data = fs::read(path).expect("Failed to read file");
    let file_size = data.len();
    println!("=== {} ({} bytes) ===\n", path, file_size);

    let mut cur = Cursor::new(data.as_slice());

    // mhbd header
    let magic = read_magic(&mut cur).expect("Failed to read mhbd magic");
    assert_eq!(&magic, b"mhbd", "Not an iTunesDB file");

    let header_size = cur.read_u32::<LittleEndian>().unwrap();
    let total_size = cur.read_u32::<LittleEndian>().unwrap();
    let _unknown1 = cur.read_u32::<LittleEndian>().unwrap();
    let db_version = cur.read_u32::<LittleEndian>().unwrap();
    let num_datasets = cur.read_u32::<LittleEndian>().unwrap();
    let db_id = cur.read_u64::<LittleEndian>().unwrap();
    let _unknown2 = cur.read_u16::<LittleEndian>().unwrap();
    let hashing_scheme = cur.read_u16::<LittleEndian>().unwrap();

    println!("mhbd:");
    println!("  header_size:    {}", header_size);
    println!("  total_size:     {} (file: {})", total_size, file_size);
    println!("  db_version:     0x{:x} ({})", db_version, db_version);
    println!("  num_datasets:   {}", num_datasets);
    println!("  db_id:          0x{:016x}", db_id);
    println!("  hashing_scheme: {}", hashing_scheme);
    println!();

    // Jump to end of mhbd header
    cur.seek(SeekFrom::Start(header_size as u64)).unwrap();

    let mut datasets = Vec::new();
    for i in 0..num_datasets {
        let ds_offset = cur.position();
        println!("--- mhsd #{} at offset {} ---", i, ds_offset);
        if let Some(ds) = parse_mhsd(&mut cur) {
            println!("  type: {} ({})", ds.ds_type, mhsd_type_name(ds.ds_type));
            println!(
                "  header_size: {}, total_size: {}",
                ds.header_size, ds.total_size
            );

            if let Some(tc) = ds.track_count {
                println!("  track_count: {} (parsed: {})", tc, ds.tracks.len());
                let show = std::cmp::min(3, ds.tracks.len());
                for (j, t) in ds.tracks.iter().take(show).enumerate() {
                    println!(
                        "    mhit[{}]: track_id={} dbid=0x{:016x} header_size={} total_size={} num_mhods={}",
                        j, t.track_id, t.dbid, t.header_size, t.total_size, t.num_mhods
                    );
                    for mhod in &t.mhods {
                        println!(
                            "      mhod type={} ({}) header_size={} total_size={}",
                            mhod.mhod_type,
                            mhod_type_name(mhod.mhod_type),
                            mhod.header_size,
                            mhod.total_size
                        );
                    }
                }
                if ds.tracks.len() > 3 {
                    println!("    ... ({} more tracks)", ds.tracks.len() - 3);
                }

                // Aggregate mhod type counts across ALL tracks
                let mut type_counts: std::collections::BTreeMap<u32, usize> =
                    std::collections::BTreeMap::new();
                let mut total_mhod_bytes: u64 = 0;
                for t in &ds.tracks {
                    for m in &t.mhods {
                        *type_counts.entry(m.mhod_type).or_insert(0) += 1;
                        total_mhod_bytes += m.total_size as u64;
                    }
                }
                println!("\n  mhod type distribution across all {} tracks:", tc);
                for (t, c) in &type_counts {
                    println!(
                        "    type {:>3} ({:<20}): {} instances",
                        t,
                        mhod_type_name(*t),
                        c
                    );
                }
                let total_mhit_header_bytes: u64 =
                    ds.tracks.iter().map(|t| t.header_size as u64).sum();
                println!(
                    "  total mhit header bytes: {}, total mhod bytes: {}",
                    total_mhit_header_bytes, total_mhod_bytes
                );
            }

            if let Some(pc) = ds.playlist_count {
                println!("  playlist_count: {} (parsed: {})", pc, ds.playlists.len());
                for (j, pl) in ds.playlists.iter().enumerate() {
                    let name = pl.playlist_name.as_deref().unwrap_or("<unnamed>");
                    println!(
                        "    mhyp[{}]: {:?} master={} header_size={} total_size={} num_mhods={} num_mhips={}",
                        j, name, pl.is_master, pl.header_size, pl.total_size, pl.num_mhods, pl.num_mhips
                    );
                    for mhod in &pl.mhods {
                        println!(
                            "      mhod type={} ({}) total_size={}",
                            mhod.mhod_type,
                            mhod_type_name(mhod.mhod_type),
                            mhod.total_size
                        );
                    }
                }
            }

            if let Some(ac) = ds.album_count {
                println!("  album_count: {}", ac);
            }

            datasets.push(ds);
        } else {
            println!("  FAILED TO PARSE");
            break;
        }
        println!();
    }

    // Summary
    println!("--- Summary ---");
    let mut total_tracks_bytes: u64 = 0;
    let mut total_playlists_bytes: u64 = 0;
    let mut total_other_bytes: u64 = 0;

    for ds in &datasets {
        match ds.ds_type {
            1 => total_tracks_bytes += ds.total_size as u64,
            2 | 3 | 5 => total_playlists_bytes += ds.total_size as u64,
            _ => total_other_bytes += ds.total_size as u64,
        }
    }
    println!("  mhbd header:      {} bytes", header_size);
    println!("  tracks datasets:   {} bytes", total_tracks_bytes);
    println!("  playlist datasets: {} bytes", total_playlists_bytes);
    println!("  other datasets:    {} bytes", total_other_bytes);
    println!(
        "  accounted:         {} bytes",
        header_size as u64 + total_tracks_bytes + total_playlists_bytes + total_other_bytes
    );
    println!("  file size:         {} bytes", file_size);
    println!(
        "  difference:        {} bytes\n",
        file_size as i64
            - (header_size as i64
                + total_tracks_bytes as i64
                + total_playlists_bytes as i64
                + total_other_bytes as i64)
    );
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: itdb_structure <file1> [file2]");
        std::process::exit(1);
    }

    for path in &args[1..] {
        analyze_file(path);
    }
}
