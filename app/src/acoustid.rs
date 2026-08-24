//! AcoustID Web Service v2 client (blocking) for fingerprint → MBID lookup.
//!
//! Library tracks already carry a Chromaprint `acoustic_id` (see
//! `src/fingerprint.rs`); coupling that with `total_time_ms` is enough to
//! identify an untagged file from scratch via `https://api.acoustid.org`.
//!
//! ## API key
//!
//! AcoustID requires a (free) application API key per request — without one
//! the API returns `{"error": {"code": 4, "message": "invalid API key"}}`.
//! Users supply theirs in `~/.config/zytunes/config.toml` as
//! `acoustid_app_key`; the tag-manager skips AcoustID dispatch entirely
//! when no key is configured rather than firing a guaranteed-failed request.
//!
//! ## Rate limit
//!
//! AcoustID's published ceiling is 3 requests/second per app key. We throttle
//! to one every ~340 ms to stay comfortably under it without inflating
//! per-call latency.
//!
//! ## Response shape (with `meta=recordings+releases`)
//!
//! ```json
//! {
//!   "status": "ok",
//!   "results": [
//!     {
//!       "id": "<acoustid-uuid>",
//!       "score": 0.95,
//!       "recordings": [
//!         {
//!           "id": "<mb-recording-mbid>",
//!           "title": "...",
//!           "artists": [{"id": "<mbid>", "name": "..."}],
//!           "releases": [{"id": "<mb-release-mbid>", "title": "..."}]
//!         }
//!       ]
//!     }
//!   ]
//! }
//! ```
//!
//! Errors come back as `{"status": "error", "error": {"code": N, "message": "..."}}`
//! which we surface as [`AcoustIdError::ApiError`].

use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Default AcoustID v2 endpoint base URL.
pub const DEFAULT_BASE_URL: &str = "https://api.acoustid.org/v2";

/// Throttle between requests targeting the canonical host.
/// AcoustID's published ceiling is 3 req/sec — 340 ms keeps us under it.
const MIN_INTERVAL: Duration = Duration::from_millis(340);

/// Errors surfaced by the AcoustID client.
#[derive(Debug)]
pub enum AcoustIdError {
    Transport(String),
    Http {
        status: u16,
        body: String,
    },
    Decode(String),
    /// API returned `{"status": "error", "error": {...}}`. `code` is the
    /// AcoustID error code (4 = invalid key, 5 = invalid client, etc.).
    ApiError {
        code: i32,
        message: String,
    },
    /// No `acoustid_app_key` configured.
    MissingAppKey,
}

impl std::fmt::Display for AcoustIdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AcoustIdError::Transport(s) => write!(f, "transport error: {s}"),
            AcoustIdError::Http { status, body } => write!(f, "HTTP {status}: {body}"),
            AcoustIdError::Decode(s) => write!(f, "decode error: {s}"),
            AcoustIdError::ApiError { code, message } => {
                write!(f, "AcoustID error {code}: {message}")
            }
            AcoustIdError::MissingAppKey => write!(
                f,
                "acoustid_app_key must be set in config to query the AcoustID API"
            ),
        }
    }
}

impl std::error::Error for AcoustIdError {}

/// Blocking AcoustID client.
pub struct AcoustIdClient {
    base_url: String,
    app_key: String,
    last_call: Mutex<Option<Instant>>,
}

impl AcoustIdClient {
    pub fn new(app_key: impl Into<String>) -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_string(),
            app_key: app_key.into(),
            last_call: Mutex::new(None),
        }
    }

    /// Override the base URL — primarily a hook for tests.
    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        let mut u = url.into();
        while u.ends_with('/') {
            u.pop();
        }
        self.base_url = u;
        self
    }

    fn throttle(&self) {
        // Instant has no invariants that a panicked thread could corrupt — if
        // another caller panicked mid-update, taking the inner value and
        // moving on is safe and keeps the worker thread alive.
        let mut last = self
            .last_call
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(prev) = *last {
            let elapsed = prev.elapsed();
            if elapsed < MIN_INTERVAL {
                std::thread::sleep(MIN_INTERVAL - elapsed);
            }
        }
        *last = Some(Instant::now());
    }

    /// Look up `(fingerprint, duration_secs)` against the AcoustID API.
    ///
    /// `meta=recordings+releases` returns enough that callers can skip a
    /// follow-up `/recording/{mbid}?inc=releases` MB round-trip for the
    /// common case — the recording hit carries its release MBIDs inline.
    pub fn lookup(
        &self,
        fingerprint: &str,
        duration_secs: u32,
    ) -> Result<Vec<AcoustIdHit>, AcoustIdError> {
        let url = self.lookup_url(fingerprint, duration_secs);
        self.throttle();
        let resp = ureq::get(&url)
            .timeout(Duration::from_secs(10))
            .set("Accept", "application/json")
            .set("User-Agent", concat!("zytunes/", env!("CARGO_PKG_VERSION")))
            .call();
        let body = match resp {
            Ok(r) => r
                .into_string()
                .map_err(|e| AcoustIdError::Transport(e.to_string()))?,
            Err(ureq::Error::Status(status, r)) => {
                let body = r.into_string().unwrap_or_default();
                return Err(AcoustIdError::Http { status, body });
            }
            Err(e) => return Err(AcoustIdError::Transport(e.to_string())),
        };
        parse_lookup_response(&body)
    }

    /// URL the next `lookup` call would issue. Exposed for diagnostic
    /// logging — same pattern as `MusicBrainzClient::search_releases_url`.
    pub fn lookup_url(&self, fingerprint: &str, duration_secs: u32) -> String {
        // `+` in a URL query is interpreted as a space — `meta=recordings+releases`
        // is fine in practice (AcoustID accepts space-separated meta keys) but
        // we encode it explicitly so the wire format matches the documented
        // `meta=recordings%2Breleases`.
        format!(
            "{}/lookup?client={}&duration={}&fingerprint={}&meta=recordings%2Breleases&format=json",
            self.base_url,
            url_encode(&self.app_key),
            duration_secs,
            url_encode(fingerprint),
        )
    }
}

/// Parse the AcoustID JSON response. Bubbles `{"status": "error", ...}`
/// payloads up as `ApiError` rather than returning success with an empty
/// hit list — those are two different conditions for the caller.
fn parse_lookup_response(body: &str) -> Result<Vec<AcoustIdHit>, AcoustIdError> {
    let parsed: AcoustIdResponse = serde_json::from_str(body).map_err(|e| {
        AcoustIdError::Decode(format!("{e} (body excerpt: {:?})", body_excerpt(body)))
    })?;
    match parsed.status.as_deref() {
        Some("ok") => Ok(parsed.results),
        Some("error") => Err(AcoustIdError::ApiError {
            code: parsed.error.as_ref().map(|e| e.code).unwrap_or(0),
            message: parsed
                .error
                .map(|e| e.message)
                .unwrap_or_else(|| "(no message)".into()),
        }),
        Some(other) => Err(AcoustIdError::Decode(format!(
            "unexpected status field: {other:?}"
        ))),
        None => Err(AcoustIdError::Decode(
            "response has no status field".to_string(),
        )),
    }
}

fn body_excerpt(body: &str) -> String {
    const LIMIT: usize = 120;
    let s: String = body
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(LIMIT)
        .collect();
    if body.chars().count() > LIMIT {
        format!("{s}…")
    } else {
        s
    }
}

fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// -------------------- Response types --------------------

#[derive(Debug, Clone, Deserialize, Serialize)]
struct AcoustIdResponse {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    results: Vec<AcoustIdHit>,
    #[serde(default)]
    error: Option<AcoustIdApiError>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct AcoustIdApiError {
    code: i32,
    message: String,
}

/// One AcoustID match. `score` is in `[0.0, 1.0]` — above 0.85 is "very
/// confident", above 0.95 is "essentially identical". A single hit can
/// carry multiple recording matches (different MB recordings sharing the
/// same fingerprint — rare but happens for live performances, mashups, etc.).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AcoustIdHit {
    pub id: String,
    #[serde(default)]
    pub score: f64,
    #[serde(default)]
    pub recordings: Vec<AcoustIdRecording>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AcoustIdRecording {
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    /// Track duration in seconds as MB has it (may differ from the
    /// caller-supplied duration by a few seconds — AcoustID tolerates
    /// up to 7s drift before refusing the match).
    #[serde(default)]
    pub duration: Option<u32>,
    #[serde(default)]
    pub artists: Vec<AcoustIdArtist>,
    #[serde(default)]
    pub releases: Vec<AcoustIdRelease>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AcoustIdArtist {
    pub id: String,
    pub name: String,
}

/// Bare release entry inside a recording. Same shape as MB's release
/// listing but the AcoustID API only ever returns `id` + `title` reliably;
/// the rest is optional. Use the `id` to fire a full MB lookup if you
/// need more detail.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AcoustIdRelease {
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
}

/// Soft cap on cached entries. Past this, the oldest insertion is evicted
/// to keep the on-disk JSON bounded. 50k is well past any realistic
/// personal-library size while still capping the JSON at a few MB.
const MAX_ENTRIES: usize = 50_000;

/// On-disk cache for AcoustID lookups. Fingerprints are stable inputs, so
/// once we've identified a track we never need to re-hit the network for it.
///
/// File format: a single JSON file with a flat `{ "<key>": [hits...] }`
/// map at `$ZYTUNES_CACHE_DIR/.zytunes-acoustid-cache.json` (or `$HOME`
/// when the env var is unset — matches the device-cache convention in
/// `src/paths.rs`). Writes go through `.tmp` + `rename` so a Ctrl+C mid-save
/// can't half-write the file.
///
/// The key is `<fingerprint>|<duration_secs_rounded>`. Duration's rounded
/// to the nearest second so a re-scan that nudges the value by milliseconds
/// still hits the cache.
///
/// **Eviction:** FIFO once `entries.len()` exceeds the (private) 50k cap.
/// The insertion order lives in `order` (in-memory only — on load it's
/// rebuilt in arbitrary HashMap iteration order, which is fine because
/// the cap still bounds growth).
pub struct AcoustIdCache {
    path: std::path::PathBuf,
    entries: std::collections::HashMap<String, Vec<AcoustIdHit>>,
    order: std::collections::VecDeque<String>,
    /// Soft cap on `entries`. Defaults to [`MAX_ENTRIES`]; tests can lower
    /// it via `with_max_entries` to exercise eviction without churning
    /// 50k inserts.
    max_entries: usize,
    log: crate::cache::Logger,
}

impl AcoustIdCache {
    /// Open (or create) the cache file under `base_dir` (typically the
    /// result of `crate::paths::device_cache_base()`).
    pub fn open(base_dir: &std::path::Path, log: crate::cache::Logger) -> Self {
        let path = base_dir.join(".zytunes-acoustid-cache.json");
        let entries: std::collections::HashMap<String, Vec<AcoustIdHit>> =
            match std::fs::read_to_string(&path) {
                Ok(body) => match serde_json::from_str(&body) {
                    Ok(map) => map,
                    Err(e) => {
                        log(&format!(
                            "acoustid cache: ignoring unparseable cache at {}: {e}",
                            path.display()
                        ));
                        std::collections::HashMap::new()
                    }
                },
                Err(_) => std::collections::HashMap::new(),
            };
        let order = entries.keys().cloned().collect();
        Self {
            path,
            entries,
            order,
            max_entries: MAX_ENTRIES,
            log,
        }
    }

    #[cfg(test)]
    pub fn with_max_entries(mut self, cap: usize) -> Self {
        self.max_entries = cap;
        self
    }

    fn key(fingerprint: &str, duration_secs: u32) -> String {
        format!("{fingerprint}|{duration_secs}")
    }

    pub fn get(&self, fingerprint: &str, duration_secs: u32) -> Option<&Vec<AcoustIdHit>> {
        self.entries.get(&Self::key(fingerprint, duration_secs))
    }

    /// Insert + persist atomically. Cache write failures are logged but do
    /// NOT propagate as errors — the in-memory entry is still good for the
    /// session, and a stale on-disk cache just means we'll re-query next
    /// launch. Eviction is FIFO once the cap is exceeded.
    pub fn insert(&mut self, fingerprint: &str, duration_secs: u32, hits: Vec<AcoustIdHit>) {
        let key = Self::key(fingerprint, duration_secs);
        // On re-insert, drop the prior position from the FIFO so the key
        // floats to the back instead of leaving a stale stub at the front.
        if self.entries.contains_key(&key) {
            if let Some(pos) = self.order.iter().position(|k| k == &key) {
                self.order.remove(pos);
            }
        }
        self.entries.insert(key.clone(), hits);
        self.order.push_back(key);
        while self.entries.len() > self.max_entries {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            } else {
                break;
            }
        }
        if let Err(e) = self.save() {
            (self.log)(&format!("acoustid cache: save failed: {e}"));
        }
    }

    /// Drop every cached entry and persist the empty cache. Exposed so a
    /// future config knob / CLI command can let the user clear it without
    /// hunting down the JSON file.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        if let Err(e) = self.save() {
            (self.log)(&format!("acoustid cache: clear-save failed: {e}"));
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn save(&self) -> std::io::Result<()> {
        let body = serde_json::to_string(&self.entries).map_err(std::io::Error::other)?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, body)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

/// Convenience: pick the best (highest-score) hit's first recording's
/// first release MBID, if any. Used by the tag-manager's lookup-first
/// chain when a single high-confidence hit comes back.
pub fn pick_top_release_mbid(hits: &[AcoustIdHit]) -> Option<String> {
    let top = hits.iter().max_by(|a, b| {
        a.score
            .partial_cmp(&b.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    })?;
    let rec = top.recordings.first()?;
    let rel = rec.releases.first()?;
    Some(rel.id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_typical_ok_response() {
        let body = r#"{
            "status": "ok",
            "results": [
                {
                    "id": "ac-uuid-1",
                    "score": 0.97,
                    "recordings": [
                        {
                            "id": "rec-mbid-1",
                            "title": "The Chain",
                            "duration": 269,
                            "artists": [{"id": "art-mbid-1", "name": "Fleetwood Mac"}],
                            "releases": [{"id": "rel-mbid-1", "title": "Rumours"}]
                        }
                    ]
                }
            ]
        }"#;
        let hits = parse_lookup_response(body).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].score > 0.96);
        assert_eq!(hits[0].recordings[0].id, "rec-mbid-1");
        assert_eq!(hits[0].recordings[0].releases[0].id, "rel-mbid-1");
    }

    #[test]
    fn parse_empty_results_is_ok() {
        let body = r#"{"status": "ok", "results": []}"#;
        let hits = parse_lookup_response(body).unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn parse_error_payload_surfaces_as_api_error() {
        let body = r#"{"status": "error", "error": {"code": 4, "message": "invalid API key"}}"#;
        let err = parse_lookup_response(body).unwrap_err();
        match err {
            AcoustIdError::ApiError { code, message } => {
                assert_eq!(code, 4);
                assert!(message.contains("invalid"));
            }
            other => panic!("expected ApiError, got {other:?}"),
        }
    }

    #[test]
    fn parse_missing_status_is_decode_error() {
        let body = r#"{"results": []}"#;
        let err = parse_lookup_response(body).unwrap_err();
        assert!(matches!(err, AcoustIdError::Decode(_)));
    }

    #[test]
    fn lookup_url_carries_required_params() {
        let c = AcoustIdClient::new("my-key");
        let url = c.lookup_url("AQADtBR=", 240);
        assert!(url.contains("client=my-key"));
        assert!(url.contains("fingerprint=AQADtBR%3D"));
        assert!(url.contains("duration=240"));
        assert!(url.contains("meta=recordings%2Breleases"));
        assert!(url.contains("format=json"));
    }

    #[test]
    fn pick_top_release_returns_highest_score() {
        let hits = vec![
            AcoustIdHit {
                id: "low".into(),
                score: 0.5,
                recordings: vec![AcoustIdRecording {
                    id: "rec-low".into(),
                    title: None,
                    duration: None,
                    artists: vec![],
                    releases: vec![AcoustIdRelease {
                        id: "rel-low".into(),
                        title: None,
                    }],
                }],
            },
            AcoustIdHit {
                id: "hi".into(),
                score: 0.95,
                recordings: vec![AcoustIdRecording {
                    id: "rec-hi".into(),
                    title: None,
                    duration: None,
                    artists: vec![],
                    releases: vec![AcoustIdRelease {
                        id: "rel-hi".into(),
                        title: None,
                    }],
                }],
            },
        ];
        assert_eq!(pick_top_release_mbid(&hits).as_deref(), Some("rel-hi"));
    }

    #[test]
    fn cache_round_trip_persists_and_reloads() {
        let dir = std::env::temp_dir().join(format!(
            "zytunes-acoustid-cache-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = crate::cache::default_logger();

        // First open: empty.
        let mut cache = AcoustIdCache::open(&dir, log.clone());
        assert!(cache.get("fp1", 240).is_none());

        // Insert + persist.
        cache.insert(
            "fp1",
            240,
            vec![AcoustIdHit {
                id: "ac1".into(),
                score: 0.9,
                recordings: vec![AcoustIdRecording {
                    id: "rec1".into(),
                    title: Some("X".into()),
                    duration: Some(240),
                    artists: vec![],
                    releases: vec![AcoustIdRelease {
                        id: "rel1".into(),
                        title: Some("Y".into()),
                    }],
                }],
            }],
        );

        // Same-process read.
        assert_eq!(cache.get("fp1", 240).map(|h| h.len()), Some(1));
        // Different duration is a different key.
        assert!(cache.get("fp1", 241).is_none());

        // Cross-process: re-open from disk.
        let cache2 = AcoustIdCache::open(&dir, log);
        let hits = cache2.get("fp1", 240).expect("entry should reload");
        assert_eq!(hits[0].recordings[0].releases[0].id, "rel1");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cache_corrupted_file_is_ignored_not_propagated() {
        // A corrupt cache file shouldn't abort startup — log it and keep going.
        let dir = std::env::temp_dir().join(format!(
            "zytunes-acoustid-corrupt-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(".zytunes-acoustid-cache.json"),
            "{this is not valid json",
        )
        .unwrap();

        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let captured_c = captured.clone();
        let log: crate::cache::Logger = std::sync::Arc::new(move |msg: &str| {
            captured_c.lock().unwrap().push(msg.to_string());
        });
        let cache = AcoustIdCache::open(&dir, log);
        assert!(cache.get("anything", 0).is_none());
        let logs = captured.lock().unwrap();
        assert!(
            logs.iter().any(|s| s.contains("unparseable")),
            "expected log mention of unparseable cache; got {logs:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn make_hit(id: &str) -> AcoustIdHit {
        AcoustIdHit {
            id: id.into(),
            score: 1.0,
            recordings: vec![],
        }
    }

    #[test]
    fn cache_evicts_oldest_when_cap_exceeded() {
        let dir = std::env::temp_dir().join(format!(
            "zytunes-acoustid-evict-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = crate::cache::default_logger();

        // Cap at 3 so 4 inserts force an eviction.
        let mut cache = AcoustIdCache::open(&dir, log).with_max_entries(3);
        cache.insert("fp-A", 100, vec![make_hit("A")]);
        cache.insert("fp-B", 100, vec![make_hit("B")]);
        cache.insert("fp-C", 100, vec![make_hit("C")]);
        assert_eq!(cache.len(), 3);
        cache.insert("fp-D", 100, vec![make_hit("D")]);
        assert_eq!(cache.len(), 3, "cap should bound entry count");
        assert!(
            cache.get("fp-A", 100).is_none(),
            "oldest insertion must be evicted first"
        );
        assert!(cache.get("fp-D", 100).is_some());
        assert!(cache.get("fp-B", 100).is_some());
        assert!(cache.get("fp-C", 100).is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cache_reinsert_floats_key_to_back() {
        // Re-inserting an existing key must NOT count toward eviction
        // pressure beyond its single slot — the key floats to the back of
        // the FIFO and the next eviction targets a different key.
        let dir = std::env::temp_dir().join(format!(
            "zytunes-acoustid-reinsert-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = crate::cache::default_logger();

        let mut cache = AcoustIdCache::open(&dir, log).with_max_entries(3);
        cache.insert("fp-A", 100, vec![make_hit("A")]);
        cache.insert("fp-B", 100, vec![make_hit("B")]);
        cache.insert("fp-C", 100, vec![make_hit("C")]);
        // Re-insert A — its position should move to back, so B is now oldest.
        cache.insert("fp-A", 100, vec![make_hit("A2")]);
        cache.insert("fp-D", 100, vec![make_hit("D")]);
        assert_eq!(cache.len(), 3);
        assert!(cache.get("fp-A", 100).is_some(), "re-inserted A survives");
        assert!(cache.get("fp-B", 100).is_none(), "B is now oldest, evicted");
        assert!(cache.get("fp-C", 100).is_some());
        assert!(cache.get("fp-D", 100).is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cache_clear_drops_entries_and_persists() {
        let dir = std::env::temp_dir().join(format!(
            "zytunes-acoustid-clear-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = crate::cache::default_logger();

        let mut cache = AcoustIdCache::open(&dir, log.clone());
        cache.insert("fp-X", 100, vec![make_hit("X")]);
        assert_eq!(cache.len(), 1);

        cache.clear();
        assert!(cache.is_empty());

        // Persisted: reopen sees empty.
        let cache2 = AcoustIdCache::open(&dir, log);
        assert!(cache2.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pick_top_release_returns_none_when_no_releases() {
        let hits = vec![AcoustIdHit {
            id: "x".into(),
            score: 0.9,
            recordings: vec![AcoustIdRecording {
                id: "rec".into(),
                title: None,
                duration: None,
                artists: vec![],
                releases: vec![],
            }],
        }];
        assert_eq!(pick_top_release_mbid(&hits), None);
    }
}
