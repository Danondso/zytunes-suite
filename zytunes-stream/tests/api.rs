//! HTTP integration tests for the streaming API (axum oneshot).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;
use zytunes::art_cache::ArtCache;
use zytunes::library::{MusicLibrary, Track};
use zytunes::local_plays::LocalPlays;
use zytunes::stems::{store_stems, StemSet, SIX_STEM_LAYOUT};
use zytunes_stream::{build_router, AppState, EngineLookup, StemHub, StemSettings};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn scratch(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "zytunes-stream-api-{}-{}-{}",
        tag,
        std::process::id(),
        n
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_pcm_wav(path: &Path, payload_tag: &[u8]) {
    // Minimal WAV with a recognizable data payload after the 44-byte header.
    let sample_rate: u32 = 44100;
    let channels: u16 = 1;
    let bits: u16 = 16;
    let pcm = {
        let mut v = vec![0u8; 64];
        let n = payload_tag.len().min(v.len());
        v[..n].copy_from_slice(&payload_tag[..n]);
        v
    };
    let data_size = pcm.len() as u32;
    let byte_rate = sample_rate * u32::from(channels) * u32::from(bits) / 8;
    let block_align = channels * bits / 8;
    let riff_size = 36 + data_size;

    let mut f = fs::File::create(path).unwrap();
    f.write_all(b"RIFF").unwrap();
    f.write_all(&riff_size.to_le_bytes()).unwrap();
    f.write_all(b"WAVE").unwrap();
    f.write_all(b"fmt ").unwrap();
    f.write_all(&16u32.to_le_bytes()).unwrap();
    f.write_all(&1u16.to_le_bytes()).unwrap();
    f.write_all(&channels.to_le_bytes()).unwrap();
    f.write_all(&sample_rate.to_le_bytes()).unwrap();
    f.write_all(&byte_rate.to_le_bytes()).unwrap();
    f.write_all(&block_align.to_le_bytes()).unwrap();
    f.write_all(&bits.to_le_bytes()).unwrap();
    f.write_all(b"data").unwrap();
    f.write_all(&data_size.to_le_bytes()).unwrap();
    f.write_all(&pcm).unwrap();
}

struct TestLib {
    tracks: Vec<Track>,
    root: String,
}

impl MusicLibrary for TestLib {
    fn artists(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self
            .tracks
            .iter()
            .map(|t| t.grouping_artist())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        v.sort_unstable();
        v
    }
    fn albums(&self) -> Vec<(&str, &str)> {
        let mut v: Vec<(&str, &str)> = self
            .tracks
            .iter()
            .map(|t| (t.grouping_artist(), t.album.as_str()))
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        v.sort_unstable();
        v
    }
    fn artist_tracks<'a>(&'a self, artist: &str) -> Box<dyn Iterator<Item = &'a Track> + 'a> {
        let a = artist.to_string();
        Box::new(
            self.tracks
                .iter()
                .filter(move |t| t.grouping_artist().eq_ignore_ascii_case(&a)),
        )
    }
    fn album_tracks<'a>(&'a self, album: &str) -> Box<dyn Iterator<Item = &'a Track> + 'a> {
        let a = album.to_string();
        Box::new(
            self.tracks
                .iter()
                .filter(move |t| t.album.eq_ignore_ascii_case(&a)),
        )
    }
    fn album_tracks_by_artist<'a>(
        &'a self,
        artist: &str,
        album: &str,
    ) -> Box<dyn Iterator<Item = &'a Track> + 'a> {
        let ar = artist.to_string();
        let al = album.to_string();
        Box::new(self.tracks.iter().filter(move |t| {
            t.grouping_artist().eq_ignore_ascii_case(&ar) && t.album.eq_ignore_ascii_case(&al)
        }))
    }
    fn tracks_by_name<'a>(&'a self, name: &str) -> Box<dyn Iterator<Item = &'a Track> + 'a> {
        let n = name.to_string();
        Box::new(
            self.tracks
                .iter()
                .filter(move |t| t.name.eq_ignore_ascii_case(&n)),
        )
    }
    fn track_count(&self) -> usize {
        self.tracks.len()
    }
    fn all_tracks(&self) -> Box<dyn Iterator<Item = &Track> + '_> {
        Box::new(self.tracks.iter())
    }
    fn music_folder(&self) -> Option<&str> {
        Some(&self.root)
    }
}

fn fixture_lib(root: &Path) -> (TestLib, u64, PathBuf) {
    let album = root.join("Radiohead").join("OK Computer");
    fs::create_dir_all(&album).unwrap();
    let wav = album.join("01 Karma Police.wav");
    write_pcm_wav(&wav, b"ZYTUNES-PAYLOAD!!!!");

    let track = Track {
        id: 42,
        name: "Karma Police".into(),
        artist: "Radiohead".into(),
        album: "OK Computer".into(),
        kind: Some("WAV".into()),
        total_time_ms: Some(1000),
        track_number: Some(1),
        sample_rate: Some(44100),
        channels: Some(1),
        file_size_bytes: Some(fs::metadata(&wav).unwrap().len()),
        location: Some(wav.to_string_lossy().into_owned()),
        ..Default::default()
    };
    (
        TestLib {
            tracks: vec![track],
            root: root.to_string_lossy().into_owned(),
        },
        42,
        wav,
    )
}

fn app_for(lib: TestLib, token: Option<&str>, art: Option<ArtCache>) -> axum::Router {
    let root = PathBuf::from(lib.music_folder().unwrap());
    let state = AppState::new(Arc::new(lib), root, art, token.map(str::to_string)).with_stems(None);
    build_router(state)
}

fn stem_settings(cache_dir: PathBuf) -> StemSettings {
    StemSettings {
        recipe: zytunes::stems::RecipeKind::Demucs,
        demucs_model: "htdemucs_6s".into(),
        cache_max_bytes: 1 << 30,
        command: None,
        cache_dir,
        engine: EngineLookup::Missing,
    }
}

fn populate_stem_cache(cache_dir: &Path, source: &Path) {
    fs::create_dir_all(cache_dir).unwrap();
    let work = cache_dir.join("produced");
    fs::create_dir_all(&work).unwrap();
    let set = StemSet::from_layout(&work, "flac", SIX_STEM_LAYOUT);
    for (i, p) in set.paths.iter().enumerate() {
        fs::write(p, format!("stem-{i}")).unwrap();
    }
    let log = zytunes::cache::default_logger();
    store_stems(cache_dir, source, "htdemucs_6s", &set, u64::MAX, &log).unwrap();
}

fn app_with_stems(lib: TestLib, hub: StemHub) -> axum::Router {
    let root = PathBuf::from(lib.music_folder().unwrap());
    let state = AppState::new(Arc::new(lib), root, None, None).with_stems(Some(hub));
    build_router(state)
}

async fn body_bytes(resp: axum::response::Response) -> Vec<u8> {
    resp.into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec()
}

#[tokio::test]
async fn health_ok() {
    let dir = scratch("health");
    let (lib, _, _) = fixture_lib(&dir);
    let app = app_for(lib, None, None);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn catalog_artists_tracks_and_detail() {
    let dir = scratch("catalog");
    let (lib, id, _) = fixture_lib(&dir);
    let app = app_for(lib, None, None);

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/artists")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let artists: Vec<String> = serde_json::from_slice(&body_bytes(resp).await).unwrap();
    assert_eq!(artists, vec!["Radiohead"]);

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/albums?artist=Radiohead")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let albums: Vec<serde_json::Value> = serde_json::from_slice(&body_bytes(resp).await).unwrap();
    assert_eq!(albums.len(), 1);
    assert_eq!(albums[0]["album"], "OK Computer");
    assert_eq!(albums[0]["track_count"], 1);
    assert_eq!(albums[0]["art_url"], "/tracks/42/art");

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/tracks?artist=Radiohead&album=OK%20Computer")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let tracks: Vec<serde_json::Value> = serde_json::from_slice(&body_bytes(resp).await).unwrap();
    assert_eq!(tracks.len(), 1);
    assert_eq!(tracks[0]["name"], "Karma Police");
    assert!(
        tracks[0]["id"].is_string(),
        "track id must be a decimal string so Dart/JS keep full u64, got {}",
        tracks[0]["id"]
    );
    assert_eq!(tracks[0]["id"], "42");

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/tracks/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let detail: serde_json::Value = serde_json::from_slice(&body_bytes(resp).await).unwrap();
    assert_eq!(detail["sample_rate"], 44100);
    assert_eq!(detail["stream_url"], format!("/tracks/{id}/stream"));
    assert_eq!(detail["file_url"], format!("/tracks/{id}/file"));

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/tracks/999999")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn search_endpoint_ranks_title() {
    let dir = scratch("search");
    let (lib, _, _) = fixture_lib(&dir);
    let app = app_for(lib, None, None);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/search?q=karma")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let hits: serde_json::Value = serde_json::from_slice(&body_bytes(resp).await).unwrap();
    assert_eq!(hits["tracks"].as_array().unwrap().len(), 1);
    assert_eq!(hits["tracks"][0]["name"], "Karma Police");
    assert!(hits["artists"].as_array().unwrap().is_empty());
    assert!(hits["albums"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn stream_full_and_range() {
    let dir = scratch("stream");
    let (lib, id, wav) = fixture_lib(&dir);
    let expected = fs::read(&wav).unwrap();
    let app = app_for(lib, None, None);

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/tracks/{id}/stream"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers().get("accept-ranges").unwrap(), "bytes");
    assert_eq!(resp.headers().get("content-type").unwrap(), "audio/wav");
    let body = body_bytes(resp).await;
    assert_eq!(body, expected);

    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/tracks/{id}/stream"))
                .header("range", "bytes=0-3")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        resp.headers()
            .get("content-range")
            .unwrap()
            .to_str()
            .unwrap(),
        format!("bytes 0-3/{}", expected.len())
    );
    let body = body_bytes(resp).await;
    assert_eq!(body, &expected[0..4]);
}

#[tokio::test]
async fn file_download_has_attachment_disposition() {
    let dir = scratch("file");
    let (lib, id, wav) = fixture_lib(&dir);
    let expected = fs::read(&wav).unwrap();
    let app = app_for(lib, None, None);

    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/tracks/{id}/file"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let disp = resp
        .headers()
        .get("content-disposition")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(disp.contains("attachment"));
    assert!(disp.contains("Karma Police.wav"));
    assert_eq!(body_bytes(resp).await, expected);
}

#[tokio::test]
async fn path_outside_library_root_returns_404() {
    let dir = scratch("escape");
    let outside = scratch("escape-outside");
    let outside_wav = outside.join("evil.wav");
    write_pcm_wav(&outside_wav, b"EVIL");

    let album = dir.join("A").join("B");
    fs::create_dir_all(&album).unwrap();
    // Track claims a location outside music_root.
    let lib = TestLib {
        tracks: vec![Track {
            id: 7,
            name: "Evil".into(),
            artist: "X".into(),
            album: "Y".into(),
            location: Some(outside_wav.to_string_lossy().into_owned()),
            ..Default::default()
        }],
        root: dir.to_string_lossy().into_owned(),
    };
    let app = app_for(lib, None, None);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/tracks/7/stream")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn art_404_without_picture_and_200_from_cache() {
    let dir = scratch("art");
    let (lib, id, wav) = fixture_lib(&dir);
    let app = app_for(lib, None, None);
    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/tracks/{id}/art"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // Second fixture with art cache populated.
    let dir2 = scratch("art-hit");
    let (lib2, id2, wav2) = fixture_lib(&dir2);
    let cache_dir = dir2.join("art-cache");
    let cache = ArtCache::new(cache_dir);
    let jpeg = vec![0xFF, 0xD8, 0xAB, 0xFF, 0xD9];
    cache
        .store("Radiohead", "OK Computer", &wav2, &jpeg)
        .unwrap();
    let app = app_for(lib2, None, Some(cache));
    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/tracks/{id2}/art"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers().get("content-type").unwrap(), "image/jpeg");
    assert_eq!(body_bytes(resp).await, jpeg);
    let _ = wav; // silence unused in first branch
}

#[tokio::test]
async fn art_png_cache_hit_declares_png() {
    let dir = scratch("art-png");
    let (lib, id, wav) = fixture_lib(&dir);
    let cache = ArtCache::new(dir.join("art-cache"));
    let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    png.extend_from_slice(b"not-a-real-png");
    cache.store("Radiohead", "OK Computer", &wav, &png).unwrap();
    let app = app_for(lib, None, Some(cache));
    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/tracks/{id}/art"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers().get("content-type").unwrap(), "image/png");
    assert_eq!(body_bytes(resp).await, png);
}

#[tokio::test]
async fn bearer_auth_required_when_configured() {
    let dir = scratch("auth");
    let (lib, _, _) = fixture_lib(&dir);
    let app = app_for(lib, Some("secret"), None);

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .header("authorization", "Bearer secret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn stems_missing_when_cache_empty() {
    let dir = scratch("stems-miss");
    let cache = dir.join("stem-cache");
    fs::create_dir_all(&cache).unwrap();
    let (lib, id, _) = fixture_lib(&dir);
    let app = app_with_stems(lib, StemHub::new(stem_settings(cache)));
    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/tracks/{id}/stems"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value = serde_json::from_slice(&body_bytes(resp).await).unwrap();
    assert_eq!(body["status"], "missing");
    assert_eq!(body["recipe"], "demucs");
    assert_eq!(body["engine_available"], false);
    assert_eq!(body["layout"].as_array().unwrap().len(), 6);
    assert_eq!(body["stems"][0]["kind"], "vocals");
    assert_eq!(
        body["stems"][0]["url"],
        format!("/tracks/{id}/stems/vocals")
    );
}

#[tokio::test]
async fn stems_ready_and_stream_cached_files() {
    let dir = scratch("stems-ready");
    let cache = dir.join("stem-cache");
    let (lib, id, wav) = fixture_lib(&dir);
    populate_stem_cache(&cache, &wav);
    let app = app_with_stems(lib, StemHub::new(stem_settings(cache)));

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/tracks/{id}/stems"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value = serde_json::from_slice(&body_bytes(resp).await).unwrap();
    assert_eq!(body["status"], "ready");
    assert_eq!(body["stems"].as_array().unwrap().len(), 6);

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/tracks/{id}/stems/vocals"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers().get("content-type").unwrap(), "audio/flac");
    assert_eq!(body_bytes(resp).await, b"stem-0");

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/tracks/{id}/stems/drums"))
                .header("range", "bytes=0-4")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(body_bytes(resp).await, b"stem-");

    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/tracks/{id}/stems/lead"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn stems_post_without_engine_fails_cleanly() {
    let dir = scratch("stems-post");
    let cache = dir.join("stem-cache");
    fs::create_dir_all(&cache).unwrap();
    let (lib, id, _) = fixture_lib(&dir);
    let app = app_with_stems(lib, StemHub::new(stem_settings(cache)));
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/tracks/{id}/stems"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value = serde_json::from_slice(&body_bytes(resp).await).unwrap();
    assert_eq!(body["status"], "failed");
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("stem engine not installed"),
        "{}",
        body["error"]
    );
}

#[tokio::test]
async fn stems_post_when_cached_is_ready() {
    let dir = scratch("stems-post-hit");
    let cache = dir.join("stem-cache");
    let (lib, id, wav) = fixture_lib(&dir);
    populate_stem_cache(&cache, &wav);
    let app = app_with_stems(lib, StemHub::new(stem_settings(cache)));
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/tracks/{id}/stems"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&body_bytes(resp).await).unwrap();
    assert_eq!(body["status"], "ready");
}

#[tokio::test]
async fn stems_unknown_track_is_404() {
    let dir = scratch("stems-404");
    let cache = dir.join("stem-cache");
    fs::create_dir_all(&cache).unwrap();
    let (lib, _, _) = fixture_lib(&dir);
    let app = app_with_stems(lib, StemHub::new(stem_settings(cache)));
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/tracks/999/stems")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn post_play_unknown_track_is_404() {
    let dir = scratch("play-404");
    let (lib, _, _) = fixture_lib(&dir);
    let app = app_for(lib, None, None);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/tracks/999999/play")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn stream_get_does_not_record_a_play() {
    let dir = scratch("play-stream");
    let (lib, id, _) = fixture_lib(&dir);
    let app = app_for(lib, None, None);

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/tracks/{id}/stream"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/tracks/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let detail: serde_json::Value = serde_json::from_slice(&body_bytes(resp).await).unwrap();
    assert!(
        detail.get("play_count").is_none() || detail["play_count"] == 0,
        "GET /stream must not bump play_count (Range seeks would inflate it), got {detail}"
    );
}

#[tokio::test]
async fn post_play_increments_count() {
    let dir = scratch("play-post");
    let (lib, id, _) = fixture_lib(&dir);
    let app = app_for(lib, None, None);

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/tracks/{id}/play"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let first: serde_json::Value = serde_json::from_slice(&body_bytes(resp).await).unwrap();
    assert_eq!(first["play_count"], 1);
    assert!(
        first["last_played_at_ms"].as_u64().unwrap() > 0,
        "last_played_at_ms should be a unix-ms timestamp, got {first}"
    );

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/tracks/{id}/play"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let second: serde_json::Value = serde_json::from_slice(&body_bytes(resp).await).unwrap();
    assert_eq!(
        second["play_count"], 1,
        "repeat POST within 30s must not increment (sidecar growth)"
    );

    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/tracks/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let detail: serde_json::Value = serde_json::from_slice(&body_bytes(resp).await).unwrap();
    assert_eq!(detail["play_count"], 1);
}

#[tokio::test]
async fn post_play_persists_to_sidecar() {
    let dir = scratch("play-persist");
    let (lib, id, _) = fixture_lib(&dir);
    let plays_path = dir.join("local-plays.json");
    let root = PathBuf::from(lib.music_folder().unwrap());
    let app = build_router(
        AppState::new(Arc::new(lib), root, None, None)
            .with_stems(None)
            .with_plays(LocalPlays::new(), Some(plays_path.clone())),
    );

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/tracks/{id}/play"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let saved = LocalPlays::load_from(&plays_path, &zytunes::cache::default_logger());
    let entry = saved
        .get(id)
        .expect("sidecar should contain the recorded play");
    assert_eq!(entry.play_count, 1);
    assert!(entry.last_played_at_ms > 0);
}
