//! Inbox discovery and canonical `{AlbumArtist}/{Album}` path helpers.
//!
//! The drop folder is a **sibling** of `music_dir` (`../Automatically Add to
//! Music`) so it never sorts among artist folders. Filing itself goes through
//! the tag-manager overlay (MusicBrainz diff + rename); this module only
//! finds settled audio files and groups them by album.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::dirlib::{is_audio_file, track_from_lofty};
use crate::library::Track;

/// Folder name next to the music library root (not inside it).
pub const INBOX_DIR_NAME: &str = "Automatically Add to Music";

/// Ignore files still being written (partial downloads).
const SETTLING: Duration = Duration::from_secs(2);

/// `{parent(music_dir)}/Automatically Add to Music`. `None` when `music_dir`
/// has no parent (`/` or a relative `.`).
pub fn default_inbox_dir(music_dir: &Path) -> Option<PathBuf> {
    music_dir.parent().map(|p| p.join(INBOX_DIR_NAME))
}

/// Replace filesystem-hostile characters in a path component.
pub fn sanitise_filename_component(s: &str) -> String {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return "Unknown".to_string();
    }
    let mut out = String::with_capacity(trimmed.len());
    for c in trimmed.chars() {
        match c {
            '/' | '\\' | '\0' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => out.push('_'),
            c if (c as u32) < 0x20 => {}
            c => out.push(c),
        }
    }
    while out.ends_with('.') || out.ends_with(' ') {
        out.pop();
    }
    if out.is_empty() {
        "Unknown".to_string()
    } else {
        out
    }
}

/// One album's worth of inbox (or reshelve) files, grouped by album artist.
#[derive(Debug, Clone)]
pub struct AlbumCluster {
    pub artist: String,
    pub album: String,
    pub tracks: Vec<Track>,
}

/// Read settled audio files under `inbox` into library `Track`s.
///
/// Pass `fingerprint = false` from the shared worker. Chromaprint belongs
/// on [`fingerprint_missing`], which the TUI runs on its own thread so a
/// pile of untagged drops does not stall connect, sync, or CD detection.
///
/// `skip` holds locations the user already dismissed. They are dropped
/// before the tag read, so a dismissed drop costs nothing per poll and is
/// never handed to the fingerprint thread.
pub fn scan_inbox(inbox: &Path, skip: &HashSet<String>, fingerprint: bool) -> Vec<Track> {
    let mut files = Vec::new();
    collect_audio(inbox, &mut files);
    let mut tracks = Vec::new();
    for path in files {
        if skip.contains(path.to_string_lossy().as_ref()) || still_settling(&path) {
            continue;
        }
        let id = crate::dirlib::hash_path(&path);
        let mut track = track_from_lofty(&path, id).unwrap_or_else(|| untagged_track(&path, id));
        if fingerprint && track.acoustic_id.is_none() {
            track.acoustic_id = crate::fingerprint::compute_fingerprint(&path);
        }
        tracks.push(track);
    }
    tracks
}

/// Fill `acoustic_id` on tracks that do not already have one.
/// Decodes audio; do not call this on the TUI worker thread.
pub fn fingerprint_missing(mut tracks: Vec<Track>) -> Vec<Track> {
    for track in &mut tracks {
        if track.acoustic_id.is_some() {
            continue;
        }
        let Some(path) = track.location.as_deref() else {
            continue;
        };
        track.acoustic_id = crate::fingerprint::compute_fingerprint(Path::new(path));
    }
    tracks
}

/// Group tracks by album artist + album so feat credits stay in one cluster.
/// Artist/album spellings that differ only by ASCII case are one cluster
/// (`Alice in Chains` / `Alice In Chains`) so `F` files the whole album
/// into the MusicBrainz folder in a single overlay.
pub fn cluster_by_album(tracks: Vec<Track>) -> Vec<AlbumCluster> {
    use std::collections::BTreeMap;
    let mut map: BTreeMap<(String, String), Vec<Track>> = BTreeMap::new();
    for t in tracks {
        let artist = t.grouping_artist();
        let album = if t.album.trim().is_empty() {
            "Unknown".to_string()
        } else {
            t.album.clone()
        };
        map.entry((artist.to_ascii_lowercase(), album.to_ascii_lowercase()))
            .or_default()
            .push(t);
    }
    map.into_values()
        .map(|tracks| {
            let artist =
                crate::library::collapse_ascii_case(tracks.iter().map(|t| t.grouping_artist()))
                    .into_iter()
                    .next()
                    .unwrap_or("Unknown")
                    .to_string();
            let album = crate::library::collapse_ascii_case(tracks.iter().map(|t| {
                if t.album.trim().is_empty() {
                    "Unknown"
                } else {
                    t.album.as_str()
                }
            }))
            .into_iter()
            .next()
            .unwrap_or("Unknown")
            .to_string();
            AlbumCluster {
                artist,
                album,
                tracks,
            }
        })
        .collect()
}

fn untagged_track(path: &Path, id: u64) -> Track {
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Unknown".into());
    Track {
        id,
        name,
        artist: "Unknown".into(),
        album: "Unknown".into(),
        location: Some(path.to_string_lossy().into_owned()),
        ..Default::default()
    }
}

fn still_settling(path: &Path) -> bool {
    let Ok(meta) = path.metadata() else {
        return false;
    };
    let Ok(modified) = meta.modified() else {
        return false;
    };
    SystemTime::now()
        .duration_since(modified)
        .map(|d| d < SETTLING)
        .unwrap_or(false)
}

fn is_temp_name(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    name.ends_with(".part")
        || name.ends_with(".download")
        || name.ends_with(".tmp")
        || name.ends_with(".crdownload")
}

fn collect_audio(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(root) else {
        return;
    };
    for ent in rd.flatten() {
        let p = ent.path();
        if p.is_dir() {
            collect_audio(&p, out);
        } else if is_audio_file(&p) && !is_temp_name(&p) {
            out.push(p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inbox_is_sibling_of_music_dir() {
        let music = PathBuf::from("/home/you/Music");
        assert_eq!(
            default_inbox_dir(&music).as_deref(),
            Some(Path::new("/home/you/Automatically Add to Music"))
        );
        assert!(default_inbox_dir(Path::new("/")).is_none());
    }

    #[test]
    fn sanitise_filename_replaces_path_separators() {
        assert_eq!(sanitise_filename_component("AC/DC"), "AC_DC");
        assert_eq!(sanitise_filename_component("a:b"), "a_b");
        assert_eq!(sanitise_filename_component(""), "Unknown");
        assert_eq!(sanitise_filename_component("Foo..."), "Foo");
        assert_eq!(sanitise_filename_component("Foo?Bar*"), "Foo_Bar_");
        assert_eq!(sanitise_filename_component("a\x01b\x02c"), "abc");
        assert_eq!(sanitise_filename_component("   "), "Unknown");
        assert_eq!(sanitise_filename_component("///"), "___");
        assert_eq!(sanitise_filename_component("Foo   "), "Foo");
        assert_eq!(sanitise_filename_component("\x01\x02\x03"), "Unknown");
    }

    #[test]
    fn cluster_keeps_feat_tracks_with_album_artist() {
        let tracks = vec![
            Track {
                artist: "*NSYNC feat. Lisa Lopes".into(),
                album: "No Strings Attached".into(),
                album_artist: Some("*NSYNC".into()),
                ..Default::default()
            },
            Track {
                artist: "*NSYNC".into(),
                album: "No Strings Attached".into(),
                album_artist: Some("*NSYNC".into()),
                ..Default::default()
            },
        ];
        let clusters = cluster_by_album(tracks);
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].artist, "*NSYNC");
        assert_eq!(clusters[0].tracks.len(), 2);
    }

    #[test]
    fn cluster_merges_ascii_case_artist_spellings() {
        let tracks = vec![
            Track {
                artist: "Alice in Chains".into(),
                album: "Dirt".into(),
                album_artist: Some("Alice in Chains".into()),
                ..Default::default()
            },
            Track {
                artist: "Alice In Chains".into(),
                album: "Dirt".into(),
                album_artist: Some("Alice In Chains".into()),
                ..Default::default()
            },
        ];
        let clusters = cluster_by_album(tracks);
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].tracks.len(), 2);
    }

    #[test]
    fn scan_inbox_skips_temp_names_and_settling_files() {
        use std::time::{Duration, SystemTime};

        let dir = std::env::temp_dir().join(format!("zytunes-inbox-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("drop.wav");
        crate::test_audio::write_sine_wav(&wav, 1);
        let past = SystemTime::now() - Duration::from_secs(10);
        std::fs::File::options()
            .write(true)
            .open(&wav)
            .unwrap()
            .set_modified(past)
            .unwrap();
        std::fs::write(dir.join("partial.wav.part"), b"nope").unwrap();

        let tracks = scan_inbox(&dir, &HashSet::new(), false);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(tracks.len(), 1);
        assert!(tracks[0]
            .location
            .as_ref()
            .is_some_and(|p| p.ends_with("drop.wav")));
    }

    #[test]
    fn scan_inbox_skips_dismissed_paths() {
        use std::time::{Duration, SystemTime};

        let dir = std::env::temp_dir().join(format!("zytunes-inbox-skip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let past = SystemTime::now() - Duration::from_secs(10);
        for name in ["keep.wav", "dismissed.wav"] {
            let wav = dir.join(name);
            crate::test_audio::write_sine_wav(&wav, 1);
            let f = std::fs::File::options().write(true).open(&wav).unwrap();
            f.set_modified(past).unwrap();
        }
        // The key is whatever a previous scan reported as `location`.
        let skip: HashSet<String> = scan_inbox(&dir, &HashSet::new(), false)
            .into_iter()
            .filter_map(|t| t.location)
            .filter(|l| l.ends_with("dismissed.wav"))
            .collect();
        assert_eq!(skip.len(), 1);

        let tracks = scan_inbox(&dir, &skip, false);
        let _ = std::fs::remove_dir_all(&dir);
        let names: Vec<_> = tracks.iter().filter_map(|t| t.location.clone()).collect();
        assert_eq!(names.len(), 1, "{names:?}");
        assert!(names[0].ends_with("keep.wav"));
    }
}
