//! Axum router and handlers.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::{Path as AxumPath, Query, Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware;
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tower_http::trace::TraceLayer;
use zytunes::art_cache::ArtCache;
use zytunes::listen_log::{ListenEvent, ListenLog};
use zytunes::local_plays::{now_unix_ms, LocalPlays};

use crate::art::load_track_art;
use crate::auth::auth_middleware;
use crate::dto::{AlbumPair, PlayRecord, SearchResults, TrackDetail, TrackSummary};
use crate::range::{content_type_for, parse_byte_range};
use crate::resolved_track_path;
use crate::search::search_library;
use crate::stems::{StemHub, StemSetDto};
use crate::LibraryHandle;

#[derive(Clone)]
pub struct AppState {
    pub library: LibraryHandle,
    pub music_root: PathBuf,
    pub art_cache: Option<Arc<ArtCache>>,
    pub token: Option<String>,
    pub stems: Option<StemHub>,
    pub plays: Arc<Mutex<LocalPlays>>,
    pub plays_path: Option<PathBuf>,
    pub listen_log: Arc<Mutex<ListenLog>>,
    play_limiter: Arc<Mutex<PlayLimiter>>,
}

impl AppState {
    pub fn new(
        library: LibraryHandle,
        music_root: PathBuf,
        art_cache: Option<ArtCache>,
        token: Option<String>,
    ) -> Self {
        Self {
            library,
            music_root,
            art_cache: art_cache.map(Arc::new),
            token,
            stems: StemHub::from_env(),
            plays: Arc::new(Mutex::new(LocalPlays::new())),
            plays_path: None,
            listen_log: Arc::new(Mutex::new(ListenLog::new())),
            play_limiter: Arc::new(Mutex::new(PlayLimiter::default())),
        }
    }

    pub fn with_stems(mut self, stems: Option<StemHub>) -> Self {
        self.stems = stems;
        self
    }

    /// Inject play-count state. Tests pass a temp sidecar path; production
    /// binds `~/.cache/zytunes/local-plays.json` so TUI and LAN clients share
    /// one history.
    pub fn with_plays(mut self, plays: LocalPlays, path: Option<PathBuf>) -> Self {
        self.plays = Arc::new(Mutex::new(plays));
        self.plays_path = path;
        self
    }

    pub fn with_listen_log(mut self, log: ListenLog) -> Self {
        self.listen_log = Arc::new(Mutex::new(log));
        self
    }
}

pub fn build_router(state: AppState) -> Router {
    let token = state.token.clone();
    Router::new()
        .route("/health", get(health))
        .route("/artists", get(list_artists))
        .route("/albums", get(list_albums))
        .route("/tracks", get(list_tracks))
        .route("/tracks/{id}", get(get_track))
        .route("/tracks/{id}/play", post(record_play))
        .route("/tracks/{id}/stream", get(stream_track))
        .route("/tracks/{id}/file", get(download_track))
        .route("/tracks/{id}/art", get(track_art))
        .route(
            "/tracks/{id}/stems",
            get(get_stems).post(post_stems).delete(delete_stems),
        )
        .route("/tracks/{id}/stems/{kind}", get(stream_stem))
        .route("/search", get(search))
        .layer(TraceLayer::new_for_http())
        .layer(middleware::from_fn_with_state(token, auth_middleware))
        .with_state(state)
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "ok": true }))
}

async fn list_artists(State(state): State<AppState>) -> Json<Vec<String>> {
    Json(
        state
            .library
            .artists()
            .into_iter()
            .map(str::to_string)
            .collect(),
    )
}

#[derive(Debug, Deserialize)]
struct AlbumQuery {
    artist: Option<String>,
}

async fn list_albums(
    State(state): State<AppState>,
    Query(q): Query<AlbumQuery>,
) -> Json<Vec<AlbumPair>> {
    Json(AlbumPair::collect(
        state.library.as_ref(),
        q.artist.as_deref(),
    ))
}

#[derive(Debug, Deserialize)]
struct TracksQuery {
    artist: Option<String>,
    album: Option<String>,
}

async fn list_tracks(
    State(state): State<AppState>,
    Query(q): Query<TracksQuery>,
) -> Json<Vec<TrackSummary>> {
    let tracks: Vec<TrackSummary> = match (q.artist.as_deref(), q.album.as_deref()) {
        (Some(artist), Some(album)) => state
            .library
            .album_tracks_by_artist(artist, album)
            .map(TrackSummary::from)
            .collect(),
        (Some(artist), None) => state
            .library
            .artist_tracks(artist)
            .map(TrackSummary::from)
            .collect(),
        (None, Some(album)) => state
            .library
            .album_tracks(album)
            .map(TrackSummary::from)
            .collect(),
        (None, None) => state.library.all_tracks().map(TrackSummary::from).collect(),
    };
    Json(tracks)
}

async fn get_track(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<u64>,
) -> Result<Json<TrackDetail>, StatusCode> {
    let track = state.library.track_by_id(id).ok_or(StatusCode::NOT_FOUND)?;
    let mut detail = TrackDetail::from_track(track);
    if let Some(entry) = lock_plays(&state.plays).get(id).cloned() {
        detail.play_count = entry.play_count;
        detail.last_played_at_ms = entry.last_played_at_ms;
    }
    Ok(Json(detail))
}

/// Client-reported play after the iTunes threshold (50% of duration or 4
/// minutes). `GET /stream` does not record: Range seeks and retries would
/// inflate counts. Repeat POSTs for the same id within 30 s are no-ops;
/// more than 60 recorded plays per minute return 429 so an open bind
/// cannot grow the sidecar without bound.
async fn record_play(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<u64>,
) -> Result<Json<PlayRecord>, StatusCode> {
    let _ = state.library.track_by_id(id).ok_or(StatusCode::NOT_FOUND)?;
    let now = now_unix_ms();
    match lock_play_limiter(&state.play_limiter).admit(id, now) {
        PlayAdmit::Duplicate => {
            let plays = lock_plays(&state.plays);
            let entry = plays.get(id);
            return Ok(Json(PlayRecord {
                play_count: entry.map(|e| e.play_count).unwrap_or(0),
                last_played_at_ms: entry.map(|e| e.last_played_at_ms).unwrap_or(0),
            }));
        }
        PlayAdmit::Limited => return Err(StatusCode::TOO_MANY_REQUESTS),
        PlayAdmit::Record => {}
    }
    let recorded = {
        let mut plays = lock_plays(&state.plays);
        plays.record_play(id, now);
        if let Some(path) = state.plays_path.as_deref() {
            plays.save_to(path);
        }
        let entry = plays.get(id).expect("just recorded");
        PlayRecord {
            play_count: entry.play_count,
            last_played_at_ms: entry.last_played_at_ms,
        }
    };
    lock_listen_log(&state.listen_log).append(ListenEvent {
        ts: now,
        id,
        completed: true,
    });
    Ok(Json(recorded))
}

const PLAY_TRACK_COOLDOWN_MS: u64 = 30_000;
const PLAY_WINDOW_MS: u64 = 60_000;
const PLAY_WINDOW_MAX: u32 = 60;

#[derive(Debug, Default)]
struct PlayLimiter {
    last_by_track: HashMap<u64, u64>,
    window_start_ms: u64,
    window_count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlayAdmit {
    Record,
    Duplicate,
    Limited,
}

impl PlayLimiter {
    fn admit(&mut self, id: u64, now_ms: u64) -> PlayAdmit {
        if let Some(&last) = self.last_by_track.get(&id) {
            if now_ms.saturating_sub(last) < PLAY_TRACK_COOLDOWN_MS {
                return PlayAdmit::Duplicate;
            }
        }
        if now_ms.saturating_sub(self.window_start_ms) >= PLAY_WINDOW_MS {
            self.window_start_ms = now_ms;
            self.window_count = 0;
        }
        if self.window_count >= PLAY_WINDOW_MAX {
            return PlayAdmit::Limited;
        }
        self.last_by_track.insert(id, now_ms);
        self.window_count += 1;
        if self.last_by_track.len() > 4096 {
            self.last_by_track
                .retain(|_, ts| now_ms.saturating_sub(*ts) < PLAY_TRACK_COOLDOWN_MS);
        }
        PlayAdmit::Record
    }
}

fn lock_plays(plays: &Mutex<LocalPlays>) -> std::sync::MutexGuard<'_, LocalPlays> {
    plays.lock().unwrap_or_else(|e| e.into_inner())
}

fn lock_listen_log(log: &Mutex<ListenLog>) -> std::sync::MutexGuard<'_, ListenLog> {
    log.lock().unwrap_or_else(|e| e.into_inner())
}

fn lock_play_limiter(limiter: &Mutex<PlayLimiter>) -> std::sync::MutexGuard<'_, PlayLimiter> {
    limiter.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Debug, Deserialize)]
struct SearchQuery {
    q: String,
}

async fn search(
    State(state): State<AppState>,
    Query(q): Query<SearchQuery>,
) -> Json<SearchResults> {
    Json(search_library(state.library.as_ref(), &q.q))
}

enum FileMode {
    Stream,
    Download,
}

async fn stream_track(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<u64>,
    req: Request,
) -> Result<Response, StatusCode> {
    serve_track_bytes(&state, id, req.headers(), FileMode::Stream).await
}

async fn download_track(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<u64>,
    req: Request,
) -> Result<Response, StatusCode> {
    serve_track_bytes(&state, id, req.headers(), FileMode::Download).await
}

async fn serve_track_bytes(
    state: &AppState,
    id: u64,
    headers: &HeaderMap,
    mode: FileMode,
) -> Result<Response, StatusCode> {
    let track = state.library.track_by_id(id).ok_or(StatusCode::NOT_FOUND)?;
    let path = resolved_track_path(track, &state.music_root).ok_or(StatusCode::NOT_FOUND)?;
    serve_path_bytes(&path, headers, mode).await
}

async fn serve_path_bytes(
    path: &Path,
    headers: &HeaderMap,
    mode: FileMode,
) -> Result<Response, StatusCode> {
    let meta = tokio::fs::metadata(path)
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;
    let file_len = meta.len();
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let content_type = content_type_for(ext);

    let range_hdr = headers.get(header::RANGE).and_then(|v| v.to_str().ok());
    let parsed = range_hdr.and_then(|r| parse_byte_range(r, file_len));

    let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("track");

    match parsed {
        Some(range) => {
            let mut file = File::open(path).await.map_err(|_| StatusCode::NOT_FOUND)?;
            file.seek(std::io::SeekFrom::Start(range.start))
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            let stream = tokio_util::io::ReaderStream::new(file.take(range.len()));
            let body = Body::from_stream(stream);
            let mut builder = Response::builder()
                .status(StatusCode::PARTIAL_CONTENT)
                .header(header::CONTENT_TYPE, content_type)
                .header(header::ACCEPT_RANGES, "bytes")
                .header(header::CONTENT_LENGTH, range.len())
                .header(
                    header::CONTENT_RANGE,
                    format!("bytes {}-{}/{}", range.start, range.end, file_len),
                );
            if matches!(mode, FileMode::Download) {
                builder = builder.header(header::CONTENT_DISPOSITION, attachment_header(filename));
            }
            Ok(builder
                .body(body)
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?)
        }
        None => {
            let file = File::open(path).await.map_err(|_| StatusCode::NOT_FOUND)?;
            let stream = tokio_util::io::ReaderStream::new(file);
            let body = Body::from_stream(stream);
            let mut builder = Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, content_type)
                .header(header::ACCEPT_RANGES, "bytes")
                .header(header::CONTENT_LENGTH, file_len);
            if matches!(mode, FileMode::Download) {
                builder = builder.header(header::CONTENT_DISPOSITION, attachment_header(filename));
            }
            Ok(builder
                .body(body)
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?)
        }
    }
}

/// Quoted `filename=` token for `Content-Disposition`. Strips quotes, path
/// separators, and non-ASCII/control characters so a hostile library name
/// cannot split the header. Falls back to `track` when nothing usable remains.
fn sanitize_download_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches(|c: char| c == ' ' || c == '.' || c == '_');
    if trimmed.is_empty() {
        "track".into()
    } else {
        trimmed.to_string()
    }
}

fn attachment_header(filename: &str) -> String {
    format!(
        "attachment; filename=\"{}\"",
        sanitize_download_filename(filename)
    )
}

fn stem_track_path(state: &AppState, id: u64) -> Result<PathBuf, StatusCode> {
    let track = state.library.track_by_id(id).ok_or(StatusCode::NOT_FOUND)?;
    resolved_track_path(track, &state.music_root).ok_or(StatusCode::NOT_FOUND)
}

async fn get_stems(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<u64>,
) -> Result<Json<StemSetDto>, StatusCode> {
    let path = stem_track_path(&state, id)?;
    let Some(hub) = &state.stems else {
        return Ok(Json(StemSetDto::unavailable()));
    };
    Ok(Json(hub.status(id, &path)))
}

async fn post_stems(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<u64>,
) -> Result<Json<StemSetDto>, StatusCode> {
    let path = stem_track_path(&state, id)?;
    let Some(hub) = &state.stems else {
        return Ok(Json(StemSetDto::unavailable()));
    };
    Ok(Json(hub.start(id, path)))
}

async fn delete_stems(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<u64>,
) -> Result<Json<StemSetDto>, StatusCode> {
    let path = stem_track_path(&state, id)?;
    let Some(hub) = &state.stems else {
        return Ok(Json(StemSetDto::unavailable()));
    };
    Ok(Json(hub.cancel(id, &path)))
}

async fn stream_stem(
    State(state): State<AppState>,
    AxumPath((id, kind)): AxumPath<(u64, String)>,
    req: Request,
) -> Result<Response, StatusCode> {
    let path = stem_track_path(&state, id)?;
    let kind = zytunes::stems::StemKind::from_file_stem(&kind).ok_or(StatusCode::NOT_FOUND)?;
    let hub = state.stems.as_ref().ok_or(StatusCode::NOT_FOUND)?;
    let stem_path = hub.stem_path(&path, kind).ok_or(StatusCode::NOT_FOUND)?;
    serve_path_bytes(&stem_path, req.headers(), FileMode::Stream).await
}

async fn track_art(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<u64>,
) -> Result<Response, StatusCode> {
    let track = state.library.track_by_id(id).ok_or(StatusCode::NOT_FOUND)?;
    let path = resolved_track_path(track, &state.music_root).ok_or(StatusCode::NOT_FOUND)?;
    let art = load_track_art(
        track.grouping_artist(),
        &track.album,
        &path,
        state.art_cache.as_deref(),
    )
    .ok_or(StatusCode::NOT_FOUND)?;
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, art.content_type)
        .header(header::CONTENT_LENGTH, art.bytes.len())
        .body(Body::from(art.bytes))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_filename_keeps_spaces_and_extension() {
        assert_eq!(
            sanitize_download_filename("Karma Police.wav"),
            "Karma Police.wav"
        );
    }

    #[test]
    fn download_filename_strips_quotes_and_separators() {
        assert_eq!(sanitize_download_filename("foo\"bar.mp3"), "foo_bar.mp3");
        assert_eq!(sanitize_download_filename("a/b\\c.flac"), "a_b_c.flac");
        assert_eq!(
            sanitize_download_filename("evil\r\nName.mp3"),
            "evil__Name.mp3"
        );
    }

    #[test]
    fn download_filename_falls_back_when_empty() {
        assert_eq!(sanitize_download_filename("..."), "track");
        assert_eq!(sanitize_download_filename("\"\""), "track");
    }

    #[test]
    fn play_limiter_treats_same_track_within_cooldown_as_duplicate() {
        let mut lim = PlayLimiter::default();
        assert_eq!(lim.admit(42, 1_000), PlayAdmit::Record);
        assert_eq!(
            lim.admit(42, 1_000 + PLAY_TRACK_COOLDOWN_MS - 1),
            PlayAdmit::Duplicate
        );
        assert_eq!(
            lim.admit(42, 1_000 + PLAY_TRACK_COOLDOWN_MS),
            PlayAdmit::Record
        );
    }

    #[test]
    fn play_limiter_caps_recorded_plays_per_window() {
        let mut lim = PlayLimiter::default();
        let start = 10_000u64;
        for id in 0..PLAY_WINDOW_MAX {
            assert_eq!(
                lim.admit(u64::from(id), start),
                PlayAdmit::Record,
                "id {id}"
            );
        }
        assert_eq!(
            lim.admit(u64::from(PLAY_WINDOW_MAX), start),
            PlayAdmit::Limited
        );
        assert_eq!(
            lim.admit(u64::from(PLAY_WINDOW_MAX), start + PLAY_WINDOW_MS),
            PlayAdmit::Record
        );
    }
}
