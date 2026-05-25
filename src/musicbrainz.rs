//! MusicBrainz Web Service v2 client (blocking) for disc lookup and metadata
//! correction.
//!
//! Targets the public `https://musicbrainz.org/ws/2` host by default. Local
//! mirrors (e.g. `musicbrainz-docker` running at `http://localhost:5000/ws/2`)
//! are supported via [`MusicBrainzClient::with_base_url`].
//!
//! ## Rate limiting and User-Agent
//!
//! Per the MusicBrainz Terms of Service the canonical host is throttled here
//! to one request per ~1.1 s and refuses to issue requests without a
//! configured `musicbrainz_user_agent`. Mirrors (non-canonical `base_url`)
//! bypass both: by convention they have no rate limit and no UA requirement.
//!
//! ## OAuth
//!
//! All read endpoints (disc lookup, recording/release search, entity
//! lookups by MBID) are publicly accessible — no auth required. OAuth is
//! reserved for *write* operations (submitting disc IDs back, edits,
//! ratings, collections). [`MusicBrainzClient::with_auth_token`] exists as
//! a forward-compatible hook; when set the token is sent as a
//! `Bearer` header so a future submission feature can reuse the same
//! client without restructuring.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Default public MusicBrainz Web Service v2 base URL.
pub const DEFAULT_BASE_URL: &str = "https://musicbrainz.org/ws/2";

/// Errors surfaced by the MusicBrainz client.
#[derive(Debug)]
pub enum MbError {
    /// Transport-level failure (DNS, TCP, TLS, IO).
    Transport(String),
    /// Server returned a non-2xx, non-404 status.
    Http { status: u16, body: String },
    /// JSON deserialization failed.
    Decode(String),
    /// No `musicbrainz_user_agent` configured while targeting the canonical host.
    MissingUserAgent,
    /// Resource (disc, recording, release) not found.
    NotFound,
}

impl std::fmt::Display for MbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MbError::Transport(s) => write!(f, "transport error: {s}"),
            MbError::Http { status, body } => write!(f, "HTTP {status}: {body}"),
            MbError::Decode(s) => write!(f, "decode error: {s}"),
            MbError::MissingUserAgent => write!(
                f,
                "musicbrainz_user_agent must be set in config to query the public MusicBrainz host"
            ),
            MbError::NotFound => write!(f, "not found in MusicBrainz"),
        }
    }
}

impl std::error::Error for MbError {}

/// Blocking MusicBrainz client. Cheap to construct, safe to share across
/// threads (rate-limit state is internally synchronised).
pub struct MusicBrainzClient {
    base_url: String,
    user_agent: Option<String>,
    auth_token: Option<String>,
    last_call: Mutex<Option<Instant>>,
}

impl MusicBrainzClient {
    /// New client targeting [`DEFAULT_BASE_URL`].
    pub fn new(user_agent: Option<String>) -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_string(),
            user_agent,
            auth_token: None,
            last_call: Mutex::new(None),
        }
    }

    /// Override the base URL. Use this for locally hosted MusicBrainz mirrors.
    /// Strips a trailing `/` so callers can pass either form.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        let mut url = base_url.into();
        while url.ends_with('/') {
            url.pop();
        }
        self.base_url = url;
        self
    }

    /// Reserved for future OAuth or personal-token support.
    pub fn with_auth_token(mut self, token: impl Into<String>) -> Self {
        self.auth_token = Some(token.into());
        self
    }

    /// True when targeting the public musicbrainz.org host; this gates both
    /// the rate limiter and the strict UA requirement.
    ///
    /// Matches the literal host (with optional `:port`) — `musicbrainz.org`
    /// only, not subdomains and not look-alikes like
    /// `musicbrainz.org.evil.example`. A misclassified URL would trip into
    /// the *stricter* path (rate-limited, UA required) so the worst-case is
    /// degraded performance, not a bypass; tightening still avoids surprising
    /// the user.
    fn is_canonical_host(&self) -> bool {
        let lowered = self.base_url.to_ascii_lowercase();
        for scheme in ["https://", "http://"] {
            let Some(rest) = lowered.strip_prefix(scheme) else {
                continue;
            };
            let host = match rest.find(['/', ':']) {
                Some(end) => &rest[..end],
                None => rest,
            };
            if host == "musicbrainz.org" {
                return true;
            }
        }
        false
    }

    fn throttle(&self) {
        if !self.is_canonical_host() {
            return;
        }
        let mut last = self
            .last_call
            .lock()
            .expect("musicbrainz throttle mutex poisoned");
        let min_interval = Duration::from_millis(1100);
        if let Some(prev) = *last {
            let elapsed = prev.elapsed();
            if elapsed < min_interval {
                std::thread::sleep(min_interval - elapsed);
            }
        }
        *last = Some(Instant::now());
    }

    fn resolve_user_agent(&self) -> Result<String, MbError> {
        match self
            .user_agent
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(ua) => Ok(ua.to_string()),
            None if self.is_canonical_host() => Err(MbError::MissingUserAgent),
            None => Ok(format!(
                "zytunes/{} (local-mirror)",
                env!("CARGO_PKG_VERSION")
            )),
        }
    }

    fn get_json<T: for<'de> Deserialize<'de>>(&self, url: &str) -> Result<T, MbError> {
        // 10s total timeout — a hung MB host (or a wedged local mirror)
        // must not block the background worker indefinitely. The worker
        // thread is shared with CD detection, so an indefinite hang
        // halts all subsequent `DetectCd` commands until the user
        // restarts the TUI.
        const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

        let ua = self.resolve_user_agent()?;
        self.throttle();
        let mut req = ureq::get(url)
            .timeout(REQUEST_TIMEOUT)
            .set("User-Agent", &ua)
            .set("Accept", "application/json");
        if let Some(token) = &self.auth_token {
            req = req.set("Authorization", &format!("Bearer {token}"));
        }
        match req.call() {
            Ok(resp) => {
                let body = resp
                    .into_string()
                    .map_err(|e| MbError::Transport(e.to_string()))?;
                parse_json(&body)
            }
            Err(ureq::Error::Status(404, _)) => Err(MbError::NotFound),
            Err(ureq::Error::Status(status, resp)) => {
                let body = resp.into_string().unwrap_or_default();
                Err(MbError::Http { status, body })
            }
            Err(e) => Err(MbError::Transport(e.to_string())),
        }
    }

    /// Look up a disc by its MusicBrainz disc ID.
    ///
    /// Includes recordings, artist credits, and release groups in a single
    /// round trip so the import overlay can render full track listings
    /// without follow-up calls.
    pub fn lookup_disc(&self, mb_disc_id: &str) -> Result<DiscLookupResponse, MbError> {
        let url = format!(
            "{}/discid/{}?inc=recordings+artist-credits+release-groups&fmt=json",
            self.base_url,
            url_encode(mb_disc_id),
        );
        self.get_json(&url)
    }
}

fn parse_json<T: for<'de> Deserialize<'de>>(body: &str) -> Result<T, MbError> {
    serde_json::from_str(body).map_err(|e| {
        // Include a body excerpt so callers debugging a malformed mirror
        // response don't have to re-issue the request to see what came back.
        let excerpt = body_excerpt(body);
        MbError::Decode(format!("{e} (body excerpt: {excerpt:?})"))
    })
}

/// Trim a JSON body to a short single-line excerpt suitable for an error.
fn body_excerpt(body: &str) -> String {
    const LIMIT: usize = 120;
    let mut chars: String = body
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(LIMIT)
        .collect();
    if body.chars().count() > LIMIT {
        chars.push('…');
    }
    chars
}

/// Percent-encode the small set of characters that can appear in MusicBrainz
/// IDs and path segments. Disc IDs are URL-safe base64 with `-`, `.`, `_`
/// substitutions — only the `+` from older encodings would need escaping, and
/// our disc-id calculator already substitutes that, but encode defensively in
/// case callers pass in raw input from elsewhere.
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

/// Top-level disc lookup response.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DiscLookupResponse {
    pub id: String,
    #[serde(default)]
    pub releases: Vec<Release>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Release {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub date: Option<String>,
    #[serde(default)]
    pub country: Option<String>,
    #[serde(default, rename = "artist-credit")]
    pub artist_credit: Vec<ArtistCredit>,
    #[serde(default)]
    pub media: Vec<Medium>,
    #[serde(default, rename = "release-group")]
    pub release_group: Option<ReleaseGroup>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ArtistCredit {
    pub name: String,
    #[serde(default)]
    pub joinphrase: Option<String>,
    #[serde(default)]
    pub artist: Option<Artist>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Artist {
    pub id: String,
    pub name: String,
    #[serde(default, rename = "sort-name")]
    pub sort_name: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Medium {
    #[serde(default)]
    pub position: Option<u32>,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default, rename = "track-count")]
    pub track_count: Option<u32>,
    #[serde(default)]
    pub tracks: Vec<Track>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Track {
    pub id: String,
    pub number: String,
    #[serde(default)]
    pub position: Option<u32>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub length: Option<u32>,
    #[serde(default)]
    pub recording: Option<Recording>,
    #[serde(default, rename = "artist-credit")]
    pub artist_credit: Vec<ArtistCredit>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Recording {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub length: Option<u32>,
    #[serde(default, rename = "artist-credit")]
    pub artist_credit: Vec<ArtistCredit>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ReleaseGroup {
    pub id: String,
    pub title: String,
    #[serde(default, rename = "primary-type")]
    pub primary_type: Option<String>,
    #[serde(default, rename = "first-release-date")]
    pub first_release_date: Option<String>,
}

/// Join an artist-credit list into a single display string honouring `joinphrase`.
pub fn render_artist_credit(credits: &[ArtistCredit]) -> String {
    let mut out = String::new();
    for ac in credits {
        out.push_str(&ac.name);
        if let Some(jp) = &ac.joinphrase {
            out.push_str(jp);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_host_detection() {
        let c = MusicBrainzClient::new(Some("ua".into()));
        assert!(c.is_canonical_host());

        let c =
            MusicBrainzClient::new(Some("ua".into())).with_base_url("http://localhost:5000/ws/2");
        assert!(!c.is_canonical_host());

        let c =
            MusicBrainzClient::new(Some("ua".into())).with_base_url("https://mb.example.org/ws/2");
        assert!(!c.is_canonical_host());
    }

    #[test]
    fn canonical_host_rejects_subdomain_lookalikes() {
        for url in [
            "https://musicbrainz.org.evil.example/ws/2",
            "https://musicbrainz.orgfoo/ws/2",
            "https://api.musicbrainz.org/ws/2",
            "https://musicbrainzborg/ws/2",
        ] {
            let c = MusicBrainzClient::new(Some("ua".into())).with_base_url(url);
            assert!(!c.is_canonical_host(), "should reject: {url}");
        }
    }

    #[test]
    fn canonical_host_accepts_port_and_no_path() {
        for url in [
            "https://musicbrainz.org",
            "https://musicbrainz.org:443/ws/2",
            "http://musicbrainz.org/ws/2",
        ] {
            let c = MusicBrainzClient::new(Some("ua".into())).with_base_url(url);
            assert!(c.is_canonical_host(), "should accept: {url}");
        }
    }

    #[test]
    fn body_excerpt_truncates_and_strips_control_chars() {
        let short = "{\"id\":\"abc\"}";
        assert_eq!(body_excerpt(short), short);

        let with_ctrl = "line1\nline2\tend";
        assert_eq!(body_excerpt(with_ctrl), "line1 line2 end");

        let long = "a".repeat(200);
        let excerpt = body_excerpt(&long);
        assert_eq!(excerpt.chars().count(), 121); // 120 chars + ellipsis
        assert!(excerpt.ends_with('…'));
    }

    #[test]
    fn decode_error_includes_body_excerpt() {
        let err = parse_json::<DiscLookupResponse>("not json at all { broken").unwrap_err();
        let s = format!("{err}");
        assert!(s.contains("body excerpt"), "missing excerpt in: {s}");
        assert!(s.contains("not json"), "missing body text in: {s}");
    }

    #[test]
    fn mirror_fallback_ua_carries_crate_version() {
        let c = MusicBrainzClient::new(None).with_base_url("http://localhost:5000/ws/2");
        let ua = c.resolve_user_agent().unwrap();
        assert!(ua.contains(env!("CARGO_PKG_VERSION")));
        assert!(ua.contains("local-mirror"));
    }

    #[test]
    fn base_url_trailing_slash_stripped() {
        let c = MusicBrainzClient::new(Some("ua".into()))
            .with_base_url("http://localhost:5000/ws/2///");
        assert_eq!(c.base_url, "http://localhost:5000/ws/2");
    }

    #[test]
    fn missing_user_agent_blocks_canonical_host() {
        let c = MusicBrainzClient::new(None);
        let err = c.resolve_user_agent().unwrap_err();
        // Bare `matches!(...)` was a no-op — it returned a bool that was
        // discarded, so a wrong variant would still pass. Wrap it.
        assert!(matches!(err, MbError::MissingUserAgent));
    }

    #[test]
    fn missing_user_agent_allowed_on_mirror() {
        let c = MusicBrainzClient::new(None).with_base_url("http://localhost:5000/ws/2");
        let ua = c.resolve_user_agent().unwrap();
        assert!(ua.contains("local-mirror"));
    }

    #[test]
    fn whitespace_user_agent_treated_as_missing() {
        let c = MusicBrainzClient::new(Some("   ".into()));
        assert!(matches!(
            c.resolve_user_agent(),
            Err(MbError::MissingUserAgent)
        ));
    }

    #[test]
    fn url_encode_passes_safe_chars() {
        assert_eq!(url_encode("abc-DEF_123.~"), "abc-DEF_123.~");
        assert_eq!(url_encode("a b"), "a%20b");
        assert_eq!(url_encode("a+b/c"), "a%2Bb%2Fc");
    }

    #[test]
    fn parses_disc_lookup_response_minimal() {
        let body = r#"{
            "id": "lSOVc5h6IXSuzcamJS1Gp4_tRuA-",
            "releases": []
        }"#;
        let parsed: DiscLookupResponse = parse_json(body).unwrap();
        assert_eq!(parsed.id, "lSOVc5h6IXSuzcamJS1Gp4_tRuA-");
        assert!(parsed.releases.is_empty());
    }

    #[test]
    fn parses_full_disc_lookup_response() {
        // Fixture mirrors the real MB JSON for a small disc, hand-trimmed.
        let body = r#"{
            "id": "TESTDISCID",
            "releases": [{
                "id": "rel-1",
                "title": "Abbey Road",
                "date": "1969-09-26",
                "country": "GB",
                "artist-credit": [{
                    "name": "The Beatles",
                    "artist": { "id": "art-1", "name": "The Beatles", "sort-name": "Beatles, The" }
                }],
                "release-group": {
                    "id": "rg-1",
                    "title": "Abbey Road",
                    "primary-type": "Album",
                    "first-release-date": "1969-09-26"
                },
                "media": [{
                    "position": 1,
                    "format": "CD",
                    "track-count": 2,
                    "tracks": [
                        {
                            "id": "trk-1",
                            "number": "1",
                            "position": 1,
                            "title": "Come Together",
                            "length": 259000,
                            "recording": {
                                "id": "rec-1",
                                "title": "Come Together",
                                "length": 259000,
                                "artist-credit": [{"name":"The Beatles"}]
                            },
                            "artist-credit": [{"name":"The Beatles"}]
                        },
                        {
                            "id": "trk-2",
                            "number": "2",
                            "position": 2,
                            "title": "Something",
                            "length": 182000,
                            "recording": {
                                "id": "rec-2",
                                "title": "Something",
                                "length": 182000,
                                "artist-credit": [{"name":"The Beatles"}]
                            },
                            "artist-credit": [{"name":"The Beatles"}]
                        }
                    ]
                }]
            }]
        }"#;
        let parsed: DiscLookupResponse = parse_json(body).unwrap();
        assert_eq!(parsed.releases.len(), 1);
        let r = &parsed.releases[0];
        assert_eq!(r.title, "Abbey Road");
        assert_eq!(r.country.as_deref(), Some("GB"));
        assert_eq!(r.artist_credit.len(), 1);
        assert_eq!(r.artist_credit[0].name, "The Beatles");
        assert_eq!(r.media.len(), 1);
        assert_eq!(r.media[0].tracks.len(), 2);
        assert_eq!(r.media[0].tracks[1].title, "Something");
        let rg = r.release_group.as_ref().unwrap();
        assert_eq!(rg.primary_type.as_deref(), Some("Album"));
    }

    #[test]
    fn render_artist_credit_joins_with_phrases() {
        let credits = vec![
            ArtistCredit {
                name: "Daft Punk".into(),
                joinphrase: Some(" feat. ".into()),
                artist: None,
            },
            ArtistCredit {
                name: "Pharrell Williams".into(),
                joinphrase: None,
                artist: None,
            },
        ];
        assert_eq!(
            render_artist_credit(&credits),
            "Daft Punk feat. Pharrell Williams"
        );
    }

    #[test]
    fn missing_user_agent_error_message_is_useful() {
        let err = MbError::MissingUserAgent;
        let s = format!("{err}");
        assert!(s.contains("musicbrainz_user_agent"));
        assert!(s.contains("config"));
    }
}
