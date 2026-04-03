//! ZMDB (Zune Metadata Database) parser.
//!
//! The Zune 30 stores its media library index in a proprietary binary format
//! called ZMDB, retrievable via vendor MTP operation 0x9217 (param=1).
//! This returns the entire device library (~700KB) in a single USB transfer,
//! replacing the need to recursively walk MTP object handles.
//!
//! Format:
//!   [ZMDB header 32b] [ZMed header 16b] [ZArr descriptors 20b each]
//!   [record data pools...] [string data...]
//!
//! Record types (high byte of index entry):
//!   0x01 — Tracks: album_ref + artist_ref + genre_ref + folder_ref + size + track_num + format + title
//!   0x06 — Albums: artist_ref + name + file_path (UTF-16LE)
//!   0x08 — Artists: name
//!   0x09 — Genres: name

use super::parse::DeviceEntry;
use std::collections::HashMap;

/// Read a little-endian u32, returning None if out of bounds.
fn u32_at(data: &[u8], off: usize) -> Option<u32> {
    let bytes = data.get(off..off + 4)?;
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// Extract the type byte and record ID from a ZMDB reference value.
fn parse_ref(val: u32) -> (u8, u32) {
    let type_byte = ((val >> 24) & 0xFF) as u8;
    let id = val & 0x00FF_FFFF;
    (type_byte, id)
}

/// Read a null-terminated ASCII string. Returns the string and the offset past the terminator.
fn read_cstring(data: &[u8], off: usize) -> (String, usize) {
    let mut s = String::new();
    let mut i = off;
    while i < data.len() && data[i] != 0 {
        if data[i] >= 0x20 && data[i] < 0x7f {
            s.push(data[i] as char);
        }
        i += 1;
    }
    // Clamp to data.len() to avoid returning an offset past the buffer.
    let next = (i + 1).min(data.len());
    (s, next)
}

struct ZmdbTrack {
    title: String,
    artist_id: u32,
    album_id: u32,
    file_size: u32,
}

struct ZmdbAlbum {
    id: u32,
    name: String,
}

struct ZmdbArtist {
    id: u32,
    name: String,
}

struct ZmdbGenre {
    #[allow(dead_code)]
    name: String,
}

pub struct Zmdb {
    tracks: Vec<ZmdbTrack>,
    albums: Vec<ZmdbAlbum>,
    artists: Vec<ZmdbArtist>,
    genres: Vec<ZmdbGenre>,
}

impl Zmdb {
    /// Parse a raw ZMDB binary blob into structured data.
    pub fn parse(data: &[u8]) -> Result<Self, String> {
        if data.len() < 0x38 {
            return Err("ZMDB data too short".into());
        }
        if &data[0..4] != b"ZMDB" {
            return Err(format!(
                "Bad ZMDB magic: {:02x}{:02x}{:02x}{:02x}",
                data[0], data[1], data[2], data[3]
            ));
        }

        // The master index is in the first ZArr descriptor at offset 0x38.
        // ZArr format: [4 magic][4 id][4 count][4 capacity][4 data_offset]
        if &data[0x38..0x3C] != b"ZArr" {
            return Err("Missing ZArr at expected offset".into());
        }
        let idx_count = u32_at(data, 0x40).ok_or("ZMDB truncated at ZArr count")? as usize;
        let idx_data_off = u32_at(data, 0x48).ok_or("ZMDB truncated at ZArr data offset")? as usize;

        let mut tracks = Vec::new();
        let mut albums = Vec::new();
        let mut artists = Vec::new();
        let mut genres = Vec::new();

        for i in 0..idx_count {
            let entry_off = idx_data_off + i * 8;
            let type_id = match u32_at(data, entry_off) {
                Some(v) => v,
                None => break,
            };
            let rec_off = match u32_at(data, entry_off + 4) {
                Some(v) => v as usize,
                None => break,
            };
            let (type_byte, rec_id) = parse_ref(type_id);

            match type_byte {
                0x01 => {
                    // Track: [album_ref 4][artist_ref 4][genre_ref 4][folder_ref 4]
                    //        [file_size 4][track_number 4][format 4][title cstring]
                    let album_id = match u32_at(data, rec_off) {
                        Some(v) => parse_ref(v).1,
                        None => continue,
                    };
                    let artist_id = match u32_at(data, rec_off + 4) {
                        Some(v) => parse_ref(v).1,
                        None => continue,
                    };
                    let file_size = match u32_at(data, rec_off + 16) {
                        Some(v) => v,
                        None => continue,
                    };
                    if rec_off + 28 > data.len() {
                        continue;
                    }
                    let (title, _) = read_cstring(data, rec_off + 28);

                    if !title.is_empty() {
                        tracks.push(ZmdbTrack {
                            title,
                            artist_id,
                            album_id,
                            file_size,
                        });
                    }
                }
                0x06 => {
                    // Album: [artist_ref 4][padding 4][folder_ref 4][name cstring]
                    if rec_off + 12 > data.len() {
                        continue;
                    }
                    let (name, _) = read_cstring(data, rec_off + 12);
                    if !name.is_empty() {
                        albums.push(ZmdbAlbum { id: rec_id, name });
                    }
                }
                0x08 => {
                    // Artist: [0x00][name cstring]
                    if rec_off + 1 > data.len() {
                        continue;
                    }
                    let (name, _) = read_cstring(data, rec_off + 1);
                    if !name.is_empty() {
                        artists.push(ZmdbArtist { id: rec_id, name });
                    }
                }
                0x09 => {
                    // Genre: [0x00][name cstring]
                    if rec_off + 1 > data.len() {
                        continue;
                    }
                    let (name, _) = read_cstring(data, rec_off + 1);
                    if !name.is_empty() {
                        genres.push(ZmdbGenre { name });
                    }
                }
                _ => {}
            }
        }

        Ok(Zmdb {
            tracks,
            albums,
            artists,
            genres,
        })
    }

    /// Convert parsed ZMDB data into DeviceEntry values compatible with
    /// the existing TUI device browser.
    ///
    /// Synthesizes paths as `Artist/Album/Title` to match the Zune's
    /// filesystem layout under `/Music`.
    pub fn to_device_entries(&self) -> Vec<DeviceEntry> {
        let artist_map: HashMap<u32, &str> = self
            .artists
            .iter()
            .map(|a| (a.id, a.name.as_str()))
            .collect();
        let album_map: HashMap<u32, &str> = self
            .albums
            .iter()
            .map(|a| (a.id, a.name.as_str()))
            .collect();

        self.tracks
            .iter()
            .map(|t| {
                let artist = artist_map.get(&t.artist_id).unwrap_or(&"Unknown Artist");
                let album = album_map.get(&t.album_id).unwrap_or(&"Unknown Album");

                DeviceEntry {
                    object_id: 0, // ZMDB IDs are not MTP object handles
                    storage_id: 0,
                    format: "MP3".to_string(),
                    size: t.file_size as u64,
                    name: format!("{}/{}/{}", artist, album, t.title),
                }
            })
            .collect()
    }

    /// Summary stats for logging.
    pub fn summary(&self) -> String {
        format!(
            "{} tracks, {} albums, {} artists, {} genres",
            self.tracks.len(),
            self.albums.len(),
            self.artists.len(),
            self.genres.len()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_zmdb(index_entries: &[(u32, u32)], record_pool: &[u8]) -> Vec<u8> {
        // Index data starts at offset 0x2D0 (matching real ZMDB)
        let idx_off: u32 = 0x2D0;
        let idx_size = (index_entries.len() * 8) as u32;
        let pool_off = idx_off + idx_size;
        let total_size = pool_off + record_pool.len() as u32;

        let mut data = vec![0u8; total_size as usize];

        // ZMDB header
        data[0..4].copy_from_slice(b"ZMDB");
        data[4..8].copy_from_slice(&1u32.to_le_bytes()); // version
        data[8..12].copy_from_slice(&total_size.to_le_bytes());
        data[0x0C..0x10].copy_from_slice(&0x2ACu32.to_le_bytes()); // header_size
        data[0x10..0x14].copy_from_slice(&(index_entries.len() as u32).to_le_bytes());

        // ZMed header at 0x20
        data[0x20..0x24].copy_from_slice(b"ZMed");
        data[0x24..0x28].copy_from_slice(&2u32.to_le_bytes());

        // ZArr descriptor at 0x38
        data[0x38..0x3C].copy_from_slice(b"ZArr");
        data[0x3C..0x40].copy_from_slice(&0x0008D000u32.to_le_bytes()); // id
        data[0x40..0x44].copy_from_slice(&(index_entries.len() as u32).to_le_bytes());
        data[0x44..0x48].copy_from_slice(&(index_entries.len() as u32).to_le_bytes()); // capacity
        data[0x48..0x4C].copy_from_slice(&idx_off.to_le_bytes());

        // Index entries
        for (i, (type_id, rec_off)) in index_entries.iter().enumerate() {
            let off = idx_off as usize + i * 8;
            data[off..off + 4].copy_from_slice(&type_id.to_le_bytes());
            // Record offset is relative to pool_off
            data[off + 4..off + 8].copy_from_slice(&(pool_off + rec_off).to_le_bytes());
        }

        // Record pool
        data[pool_off as usize..].copy_from_slice(record_pool);

        data
    }

    fn make_track_record(
        album_id: u32,
        artist_id: u32,
        genre_id: u32,
        size: u32,
        track_num: u32,
        title: &str,
    ) -> Vec<u8> {
        let mut rec = Vec::new();
        rec.extend_from_slice(&((album_id & 0x00FFFFFF) | 0x06000000).to_le_bytes()); // album_ref
        rec.extend_from_slice(&((artist_id & 0x00FFFFFF) | 0x08000000).to_le_bytes()); // artist_ref
        rec.extend_from_slice(&((genre_id & 0x00FFFFFF) | 0x09000000).to_le_bytes()); // genre_ref
        rec.extend_from_slice(&0x05000001u32.to_le_bytes()); // folder_ref
        rec.extend_from_slice(&size.to_le_bytes());
        rec.extend_from_slice(&track_num.to_le_bytes());
        rec.extend_from_slice(&0x09300000u32.to_le_bytes()); // format
        rec.extend_from_slice(title.as_bytes());
        rec.push(0); // null terminator
        rec
    }

    fn make_artist_record(name: &str) -> Vec<u8> {
        let mut rec = vec![0x00]; // leading zero byte
        rec.extend_from_slice(name.as_bytes());
        rec.push(0);
        rec
    }

    fn make_album_record(artist_id: u32, name: &str) -> Vec<u8> {
        let mut rec = Vec::new();
        rec.extend_from_slice(&((artist_id & 0x00FFFFFF) | 0x08000000).to_le_bytes()); // artist_ref
        rec.extend_from_slice(&0u32.to_le_bytes()); // padding
        rec.extend_from_slice(&0x05000001u32.to_le_bytes()); // folder_ref
        rec.extend_from_slice(name.as_bytes());
        rec.push(0);
        rec
    }

    fn make_genre_record(name: &str) -> Vec<u8> {
        let mut rec = vec![0x00];
        rec.extend_from_slice(name.as_bytes());
        rec.push(0);
        rec
    }

    #[test]
    fn parse_minimal_zmdb() {
        let artist_rec = make_artist_record("Weezer");
        let album_rec = make_album_record(1, "Blue Album");
        let genre_rec = make_genre_record("Rock");
        let track_rec = make_track_record(1, 1, 1, 5_000_000, 3, "Say It Ain't So");

        let artist_off = 0u32;
        let album_off = artist_off + artist_rec.len() as u32;
        let genre_off = album_off + album_rec.len() as u32;
        let track_off = genre_off + genre_rec.len() as u32;

        let mut pool = Vec::new();
        pool.extend_from_slice(&artist_rec);
        pool.extend_from_slice(&album_rec);
        pool.extend_from_slice(&genre_rec);
        pool.extend_from_slice(&track_rec);

        let data = make_zmdb(
            &[
                (0x08000001, artist_off),
                (0x06000001, album_off),
                (0x09000001, genre_off),
                (0x01000001, track_off),
            ],
            &pool,
        );

        let zmdb = Zmdb::parse(&data).unwrap();
        assert_eq!(zmdb.artists.len(), 1);
        assert_eq!(zmdb.artists[0].name, "Weezer");
        assert_eq!(zmdb.albums.len(), 1);
        assert_eq!(zmdb.albums[0].name, "Blue Album");
        assert_eq!(zmdb.genres.len(), 1);
        assert_eq!(zmdb.genres[0].name, "Rock");
        assert_eq!(zmdb.tracks.len(), 1);
        assert_eq!(zmdb.tracks[0].title, "Say It Ain't So");
        assert_eq!(zmdb.tracks[0].file_size, 5_000_000);
    }

    #[test]
    fn to_device_entries_synthesizes_paths() {
        let artist_rec = make_artist_record("AFI");
        let album_rec = make_album_record(1, "Sing the Sorrow");
        let genre_rec = make_genre_record("Punk");
        let track_rec = make_track_record(1, 1, 1, 3_000_000, 1, "Miseria Cantare");

        let a_off = 0u32;
        let al_off = a_off + artist_rec.len() as u32;
        let g_off = al_off + album_rec.len() as u32;
        let t_off = g_off + genre_rec.len() as u32;

        let mut pool = Vec::new();
        pool.extend_from_slice(&artist_rec);
        pool.extend_from_slice(&album_rec);
        pool.extend_from_slice(&genre_rec);
        pool.extend_from_slice(&track_rec);

        let data = make_zmdb(
            &[
                (0x08000001, a_off),
                (0x06000001, al_off),
                (0x09000001, g_off),
                (0x01000001, t_off),
            ],
            &pool,
        );

        let zmdb = Zmdb::parse(&data).unwrap();
        let entries = zmdb.to_device_entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "AFI/Sing the Sorrow/Miseria Cantare");
        assert_eq!(entries[0].size, 3_000_000);
        assert!(!entries[0].is_dir());
    }

    #[test]
    fn bad_magic_rejected() {
        let data = vec![0u8; 0x50];
        assert!(Zmdb::parse(&data).is_err());
    }

    #[test]
    fn too_short_rejected() {
        assert!(Zmdb::parse(&[]).is_err());
        assert!(Zmdb::parse(&[0; 10]).is_err());
    }

    #[test]
    fn unresolvable_refs_fall_back_to_unknown() {
        // Track references artist_id=99 and album_id=99, which don't exist.
        let track_rec = make_track_record(99, 99, 99, 1_000_000, 1, "Orphan Track");

        let data = make_zmdb(&[(0x01000001, 0)], &track_rec);

        let zmdb = Zmdb::parse(&data).unwrap();
        assert_eq!(zmdb.tracks.len(), 1);

        let entries = zmdb.to_device_entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "Unknown Artist/Unknown Album/Orphan Track");
    }
}
