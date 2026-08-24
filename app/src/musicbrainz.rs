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
    /// Includes recordings, artist credits, release groups, ISRCs and label
    /// info in a single round trip so the import overlay can render full
    /// track listings and the rip pipeline can write Picard-equivalent tags
    /// without follow-up calls. The `isrcs` inc lands per-recording; `labels`
    /// adds `label-info[*].catalog-number` + `label-info[*].label.name` at
    /// the release level.
    pub fn lookup_disc(&self, mb_disc_id: &str) -> Result<DiscLookupResponse, MbError> {
        let url = format!(
            "{}/discid/{}?inc=recordings+artist-credits+release-groups+isrcs+labels+genres&fmt=json",
            self.base_url,
            url_encode(mb_disc_id),
        );
        self.get_json(&url)
    }

    /// Search MB releases by artist + album.
    ///
    /// Builds a Lucene query of the form `release:"<album>" AND artist:"<artist>"`
    /// with the special characters that the Lucene parser treats as operators
    /// escaped (so `AC/DC` doesn't blow up). Returns up to `limit` hits ranked
    /// by MB's relevance score.
    pub fn search_releases(
        &self,
        artist: &str,
        album: &str,
        limit: u32,
    ) -> Result<ReleaseSearchResponse, MbError> {
        self.get_json(&self.search_releases_url(artist, album, limit))
    }

    /// The URL that [`Self::search_releases`] would request. Exposed so callers
    /// can log it for diagnostics without having to reconstruct it
    /// themselves (and risk drifting out of sync with the real impl).
    pub fn search_releases_url(&self, artist: &str, album: &str, limit: u32) -> String {
        // Defensive trim: callers should already pass clean values (dirlib
        // normalises identity tags at scan time), but a stale cache or a
        // future caller writing through this path could still hand us
        // "311                           " and get zero Solr hits. Trim
        // belt-and-suspenders style.
        let artist = artist.trim();
        let album = album.trim();
        let query = format!(
            "release:\"{}\" AND artist:\"{}\"",
            lucene_escape(album),
            lucene_escape(artist),
        );
        format!(
            "{}/release/?query={}&limit={}&fmt=json",
            self.base_url,
            url_encode(&query),
            limit,
        )
    }

    /// Full release lookup by MBID.
    ///
    /// Includes the same `inc` set as `lookup_disc` so callers get track
    /// listings, artist credits, release groups, per-recording ISRCs, and
    /// label info in one round trip.
    pub fn lookup_release_full(&self, mbid: &str) -> Result<Release, MbError> {
        let url = format!(
            "{}/release/{}?inc=recordings+artist-credits+release-groups+isrcs+labels+genres&fmt=json",
            self.base_url,
            url_encode(mbid),
        );
        self.get_json(&url)
    }

    /// Look up a recording by MBID, including the releases it appears on
    /// (`inc=releases+artist-credits`). Used as a fallback for AcoustID
    /// hits where the recording matched but the AcoustID response's
    /// `recordings[].releases[]` list was empty — MB itself usually knows
    /// the recording→release links the AcoustID submission didn't include.
    pub fn lookup_recording_with_releases(
        &self,
        mbid: &str,
    ) -> Result<RecordingLookupResponse, MbError> {
        let url = format!(
            "{}/recording/{}?inc=releases+artist-credits&fmt=json",
            self.base_url,
            url_encode(mbid),
        );
        self.get_json(&url)
    }
}

/// Escape a string for use inside a Lucene quoted phrase. We always wrap the
/// user value in double quotes in the query, so only the two characters that
/// terminate or escape inside a quoted phrase need escaping: `"` and `\`.
fn lucene_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' | '"' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
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

/// Top-level release-search response (`/release/?query=...`).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ReleaseSearchResponse {
    #[serde(default)]
    pub releases: Vec<ReleaseSearchHit>,
}

/// A single hit in `ReleaseSearchResponse::releases`. The search endpoint
/// returns a leaner shape than the full release lookup (no `media[*].tracks`),
/// so this is a distinct struct rather than reusing [`Release`].
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ReleaseSearchHit {
    pub id: String,
    /// MB relevance score (0-100). Higher = better match.
    #[serde(default)]
    pub score: u32,
    pub title: String,
    #[serde(default)]
    pub date: Option<String>,
    #[serde(default)]
    pub country: Option<String>,
    #[serde(default, rename = "artist-credit")]
    pub artist_credit: Vec<ArtistCredit>,
    #[serde(default, rename = "release-group")]
    pub release_group: Option<ReleaseGroup>,
    /// Per-medium metadata WITHOUT track listings. The search endpoint surfaces
    /// `track-count` here but not the full `tracks` array.
    #[serde(default)]
    pub media: Vec<MediumSummary>,
    /// Aggregate track count across all media — present on hits where MB has
    /// computed it. Display falls back to summing `media[*].track_count`.
    #[serde(default, rename = "track-count")]
    pub track_count: Option<u32>,
    #[serde(default, rename = "label-info")]
    pub label_info: Vec<LabelInfo>,
}

/// Search-endpoint medium shape: just position/format/track-count. The full
/// release lookup uses [`Medium`] which carries `tracks: Vec<Track>`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MediumSummary {
    #[serde(default)]
    pub position: Option<u32>,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default, rename = "track-count")]
    pub track_count: Option<u32>,
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
    /// Bar/UPC. MB returns empty string when unknown (not null), so the
    /// rip-time `set_string` early-return on empty values handles that.
    #[serde(default)]
    pub barcode: Option<String>,
    /// Amazon ASIN. Sometimes `null`, sometimes absent — `Option` either way.
    #[serde(default)]
    pub asin: Option<String>,
    /// MB release status: "Official", "Promotion", "Bootleg", "Pseudo-Release".
    #[serde(default)]
    pub status: Option<String>,
    /// Physical packaging: "Jewel Case", "Digipak", "Gatefold Cover", etc.
    #[serde(default)]
    pub packaging: Option<String>,
    #[serde(default, rename = "text-representation")]
    pub text_representation: Option<TextRepresentation>,
    /// Label + catalog-number pairs (requires `inc=labels`). May be empty
    /// for self-releases or labelless promo discs.
    #[serde(default, rename = "label-info")]
    pub label_info: Vec<LabelInfo>,
    /// Release-level genres (requires `inc=genres`). Usually empty —
    /// release-group genres are the populated source. Kept as a fallback
    /// for the rare case where only the release carries genre votes.
    #[serde(default)]
    pub genres: Vec<MbGenre>,
}

/// Language + script the release's text is in (ISO 639-3 / ISO 15924).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TextRepresentation {
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub script: Option<String>,
}

/// One label / catalog-number pairing for a release.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LabelInfo {
    #[serde(default, rename = "catalog-number")]
    pub catalog_number: Option<String>,
    #[serde(default)]
    pub label: Option<Label>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Label {
    pub id: String,
    pub name: String,
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
    /// Phonographic ISRCs. MB stores them on the recording (the canonical
    /// location) — `track.isrcs` is not populated in practice on disc lookup.
    #[serde(default)]
    pub isrcs: Vec<String>,
}

/// Top-level shape of `/recording/{mbid}?inc=releases+artist-credits`.
/// Matches the search-hit shape closely so the tag-manager can convert
/// the contained releases into the same `ReleaseSearchHit` rows as the
/// AcoustID and MB-search paths.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RecordingLookupResponse {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default, rename = "artist-credit")]
    pub artist_credit: Vec<ArtistCredit>,
    /// Releases this recording appears on. Each release here carries the
    /// lighter "search hit" shape (no `media[*].tracks`), so the
    /// tag-manager round-trips through `lookup_release_full` for the
    /// user-picked one.
    #[serde(default)]
    pub releases: Vec<RecordingReleaseRef>,
}

/// Release entry inside a `RecordingLookupResponse`. Strictly less data
/// than a full `Release` — the recording-lookup endpoint omits `media[*]`
/// and `artist-credit` at the release level even with `inc=artist-credits`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RecordingReleaseRef {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub date: Option<String>,
    #[serde(default)]
    pub country: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default, rename = "release-group")]
    pub release_group: Option<ReleaseGroup>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ReleaseGroup {
    pub id: String,
    pub title: String,
    #[serde(default, rename = "primary-type")]
    pub primary_type: Option<String>,
    #[serde(default, rename = "first-release-date")]
    pub first_release_date: Option<String>,
    /// Crowd-curated genres for the release group (requires `inc=genres`).
    /// Ranked by `count` — the most-voted genre is generally what Picard
    /// would write. Picard prefers release-group genres over release-level
    /// because they aggregate across all releases in the group.
    #[serde(default)]
    pub genres: Vec<MbGenre>,
}

/// One MB genre. Score by `count` (number of users who tagged the entity
/// with this genre) when picking a single representative.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MbGenre {
    pub name: String,
    #[serde(default)]
    pub count: u32,
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
        // Carries one ISRC + one label-info + barcode/asin/status/packaging
        // so the Phase A extensions round-trip in a realistic shape.
        let body = r#"{
            "id": "TESTDISCID",
            "releases": [{
                "id": "rel-1",
                "title": "Abbey Road",
                "date": "1969-09-26",
                "country": "GB",
                "barcode": "077774644020",
                "asin": "B000002UAS",
                "status": "Official",
                "packaging": "Jewel Case",
                "text-representation": {"language": "eng", "script": "Latn"},
                "label-info": [{
                    "catalog-number": "PCS 7088",
                    "label": {"id": "lbl-1", "name": "Apple"}
                }],
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
                                "isrcs": ["GBAYE6900001"],
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
                                "isrcs": [],
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
        assert_eq!(r.barcode.as_deref(), Some("077774644020"));
        assert_eq!(r.asin.as_deref(), Some("B000002UAS"));
        assert_eq!(r.status.as_deref(), Some("Official"));
        assert_eq!(r.packaging.as_deref(), Some("Jewel Case"));
        let tr = r.text_representation.as_ref().unwrap();
        assert_eq!(tr.language.as_deref(), Some("eng"));
        assert_eq!(tr.script.as_deref(), Some("Latn"));
        assert_eq!(r.label_info.len(), 1);
        assert_eq!(r.label_info[0].catalog_number.as_deref(), Some("PCS 7088"));
        assert_eq!(r.label_info[0].label.as_ref().unwrap().name, "Apple");
        assert_eq!(r.artist_credit.len(), 1);
        assert_eq!(r.artist_credit[0].name, "The Beatles");
        assert_eq!(r.media.len(), 1);
        assert_eq!(r.media[0].tracks.len(), 2);
        assert_eq!(r.media[0].tracks[1].title, "Something");
        let rg = r.release_group.as_ref().unwrap();
        assert_eq!(rg.primary_type.as_deref(), Some("Album"));
        // Recording-level ISRCs land where MB actually puts them.
        let rec0 = r.media[0].tracks[0].recording.as_ref().unwrap();
        assert_eq!(rec0.isrcs, vec!["GBAYE6900001".to_string()]);
        let rec1 = r.media[0].tracks[1].recording.as_ref().unwrap();
        assert!(rec1.isrcs.is_empty());
    }

    #[test]
    fn parses_isrc_list_on_recording() {
        let body = r#"{
            "id": "rec-1",
            "title": "T",
            "isrcs": ["USRC17607839", "GBN9Y2200015"],
            "artist-credit": []
        }"#;
        let parsed: Recording = parse_json(body).unwrap();
        assert_eq!(parsed.isrcs, vec!["USRC17607839", "GBN9Y2200015"]);
    }

    #[test]
    fn parses_label_info_with_catalog_number() {
        let body = r#"{
            "id": "rel-1",
            "title": "X",
            "label-info": [{
                "catalog-number": "SHVL 804",
                "label": {"id": "lbl-1", "name": "Harvest"}
            }]
        }"#;
        let parsed: Release = parse_json(body).unwrap();
        assert_eq!(parsed.label_info.len(), 1);
        assert_eq!(
            parsed.label_info[0].catalog_number.as_deref(),
            Some("SHVL 804")
        );
        let lbl = parsed.label_info[0].label.as_ref().unwrap();
        assert_eq!(lbl.id, "lbl-1");
        assert_eq!(lbl.name, "Harvest");
    }

    #[test]
    fn parses_label_info_without_label_or_catalog_number() {
        // Some MB releases have an entry with only one or neither field
        // populated. Both `label` and `catalog_number` are Optional.
        let body = r#"{
            "id": "rel-1",
            "title": "X",
            "label-info": [{}]
        }"#;
        let parsed: Release = parse_json(body).unwrap();
        assert_eq!(parsed.label_info.len(), 1);
        assert!(parsed.label_info[0].catalog_number.is_none());
        assert!(parsed.label_info[0].label.is_none());
    }

    #[test]
    fn parses_barcode_asin_status_packaging() {
        let body = r#"{
            "id": "rel-1",
            "title": "X",
            "barcode": "077774644020",
            "asin": "B000002UAS",
            "status": "Official",
            "packaging": "Gatefold Cover"
        }"#;
        let parsed: Release = parse_json(body).unwrap();
        assert_eq!(parsed.barcode.as_deref(), Some("077774644020"));
        assert_eq!(parsed.asin.as_deref(), Some("B000002UAS"));
        assert_eq!(parsed.status.as_deref(), Some("Official"));
        assert_eq!(parsed.packaging.as_deref(), Some("Gatefold Cover"));
    }

    #[test]
    fn parses_text_representation() {
        let body = r#"{
            "id": "rel-1",
            "title": "X",
            "text-representation": {"language": "eng", "script": "Latn"}
        }"#;
        let parsed: Release = parse_json(body).unwrap();
        let tr = parsed.text_representation.unwrap();
        assert_eq!(tr.language.as_deref(), Some("eng"));
        assert_eq!(tr.script.as_deref(), Some("Latn"));
    }

    #[test]
    fn parses_release_missing_optional_fields() {
        // Minimum-shape release: MB can omit barcode/asin/status/packaging/
        // text-representation/label-info entirely. All must default cleanly
        // without erroring.
        let body = r#"{ "id": "rel-1", "title": "X" }"#;
        let parsed: Release = parse_json(body).unwrap();
        assert!(parsed.barcode.is_none());
        assert!(parsed.asin.is_none());
        assert!(parsed.status.is_none());
        assert!(parsed.packaging.is_none());
        assert!(parsed.text_representation.is_none());
        assert!(parsed.label_info.is_empty());
    }

    #[test]
    fn parses_empty_string_barcode_distinct_from_missing() {
        // MB returns `"barcode": ""` (not null, not omitted) when there's
        // no barcode for the release. Verify we faithfully preserve the
        // distinction so the tag writer's empty-string skip works.
        let body = r#"{ "id": "rel-1", "title": "X", "barcode": "" }"#;
        let parsed: Release = parse_json(body).unwrap();
        assert_eq!(parsed.barcode.as_deref(), Some(""));
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
    fn lucene_escape_handles_quotes_and_backslashes() {
        assert_eq!(lucene_escape(r#"AC/DC"#), r#"AC/DC"#);
        assert_eq!(lucene_escape(r#"a"b"#), r#"a\"b"#);
        assert_eq!(lucene_escape(r#"a\b"#), r#"a\\b"#);
        // Lucene operators inside quoted phrases are taken literally so they
        // don't need escaping — only the two phrase-terminators do.
        assert_eq!(lucene_escape("a + b - c"), "a + b - c");
    }

    #[test]
    fn search_releases_url_trims_padded_inputs() {
        // Regression: legacy iTunes-style m4a tags surface artist/album
        // with trailing-space padding. The MB query builder must strip it
        // so the literal phrase query "311" matches, not the doomed
        // "311                           " which Solr would never satisfy.
        let c = MusicBrainzClient::new(Some("ua".into()));
        let padded = c.search_releases_url("311                           ", "  ", 12);
        let clean = c.search_releases_url("311", "", 12);
        assert_eq!(padded, clean);
        assert!(padded.contains("artist%3A%22311%22"));
    }

    #[test]
    fn search_releases_parses_typical_response() {
        let body = r#"{
            "created": "2024-01-01T00:00:00.000Z",
            "count": 2,
            "offset": 0,
            "releases": [
                {
                    "id": "rel-1",
                    "score": 100,
                    "title": "Abbey Road",
                    "date": "1969-09-26",
                    "country": "GB",
                    "track-count": 17,
                    "artist-credit": [{"name": "The Beatles"}],
                    "media": [{"position": 1, "format": "CD", "track-count": 17}]
                },
                {
                    "id": "rel-2",
                    "score": 95,
                    "title": "Abbey Road",
                    "date": "1987",
                    "country": "US",
                    "artist-credit": [{"name": "The Beatles"}],
                    "media": [{"position": 1, "format": "CD", "track-count": 17}]
                }
            ]
        }"#;
        let parsed: ReleaseSearchResponse = parse_json(body).unwrap();
        assert_eq!(parsed.releases.len(), 2);
        assert_eq!(parsed.releases[0].score, 100);
        assert_eq!(parsed.releases[0].country.as_deref(), Some("GB"));
        assert_eq!(parsed.releases[0].track_count, Some(17));
        assert_eq!(parsed.releases[0].media[0].track_count, Some(17));
    }

    #[test]
    fn search_releases_handles_no_results() {
        let body =
            r#"{"created": "2024-01-01T00:00:00.000Z", "count": 0, "offset": 0, "releases": []}"#;
        let parsed: ReleaseSearchResponse = parse_json(body).unwrap();
        assert!(parsed.releases.is_empty());
    }

    #[test]
    fn search_releases_parses_live_mb_response() {
        // Regression: the search was returning 0 hits in the TUI for queries
        // that MB serves 90 hits to via curl. The culprit was a struct shape
        // mismatch we missed in the unit fixtures — the live MB shape for a
        // search hit's `media[0]` lacks a `position` field, includes an `id`
        // we don't model, and adds `disc-count`. If any required field is
        // missing or any type mismatches, `parse_json` errors and the entire
        // response (all 90 hits) is dropped, surfacing as 0 hits in the UI.
        //
        // This fixture is captured verbatim from a live MB search
        // (release:"Rumours" AND artist:"Fleetwood Mac", limit=1).
        let body = r#"{"created":"2026-05-26T01:37:12.691Z","count":90,"offset":0,"releases":[{"id":"7ec069c0-4424-3169-8ed0-d5e2473e0e84","score":100,"status-id":"4e304316-386d-3409-af2e-78857eec5cfe","packaging-id":"f7101ce3-0384-39ce-9fde-fbbd0044d35f","artist-credit-id":"594d8902-324d-39fb-8c34-ebe2fbb5a8d7","count":1,"title":"Rumours","status":"Official","packaging":"Cardboard/Paper Sleeve","text-representation":{"language":"eng","script":"Latn"},"artist-credit":[{"name":"Fleetwood Mac","artist":{"id":"bd13909f-1c29-4c27-a874-d4aaf27c5b1a","name":"Fleetwood Mac","sort-name":"Fleetwood Mac"}}],"release-group":{"id":"416bb5e5-c7d1-3977-8fd7-7c9daf6c2be6","type-id":"f529b476-6e62-324f-b0aa-1f3e33d313fc","primary-type-id":"f529b476-6e62-324f-b0aa-1f3e33d313fc","title":"Rumours","primary-type":"Album"},"date":"1977","country":"GB","release-events":[{"date":"1977","area":{"id":"8a754a16-0027-3a29-b6d7-2b40ea0481ed","name":"United Kingdom","sort-name":"United Kingdom","iso-3166-1-codes":["GB"]}}],"label-info":[{"catalog-number":"K 56344","label":{"id":"c595c289-47ce-4fba-b999-b87503e8cb71","name":"Warner Bros. Records"}}],"track-count":11,"media":[{"id":"e82a3532-6d1c-3fd2-b8a9-5790783f8a18","format":"12\" Vinyl","disc-count":0,"track-count":11}]}]}"#;
        let parsed: ReleaseSearchResponse = parse_json(body).unwrap();
        assert_eq!(parsed.releases.len(), 1);
        let r = &parsed.releases[0];
        assert_eq!(r.title, "Rumours");
        assert_eq!(r.score, 100);
        assert_eq!(r.country.as_deref(), Some("GB"));
        assert_eq!(r.track_count, Some(11));
        assert_eq!(r.media.len(), 1);
        assert_eq!(r.media[0].format.as_deref(), Some("12\" Vinyl"));
        assert_eq!(r.media[0].track_count, Some(11));
        assert_eq!(r.label_info.len(), 1);
        assert_eq!(r.label_info[0].catalog_number.as_deref(), Some("K 56344"));
        assert_eq!(
            r.label_info[0].label.as_ref().unwrap().name,
            "Warner Bros. Records"
        );
    }

    #[test]
    fn lookup_release_full_round_trips_tracks() {
        // The full release lookup carries the same shape as a disc lookup's
        // `releases[0]`. Use a minimal fixture with one track + recording to
        // confirm `inc=recordings+...` round-trips into our `Release` type.
        let body = r#"{
            "id": "rel-1",
            "title": "X",
            "date": "2000",
            "country": "US",
            "artist-credit": [{"name": "A"}],
            "media": [{
                "position": 1,
                "format": "CD",
                "track-count": 1,
                "tracks": [{
                    "id": "trk-1",
                    "number": "1",
                    "position": 1,
                    "title": "T",
                    "length": 100000,
                    "recording": {
                        "id": "rec-1",
                        "title": "T",
                        "length": 100000,
                        "isrcs": ["USRC17600001"],
                        "artist-credit": [{"name": "A"}]
                    },
                    "artist-credit": [{"name": "A"}]
                }]
            }]
        }"#;
        let parsed: Release = parse_json(body).unwrap();
        assert_eq!(parsed.id, "rel-1");
        assert_eq!(parsed.media.len(), 1);
        assert_eq!(parsed.media[0].tracks.len(), 1);
        let rec = parsed.media[0].tracks[0].recording.as_ref().unwrap();
        assert_eq!(rec.isrcs, vec!["USRC17600001".to_string()]);
    }

    #[test]
    fn missing_user_agent_error_message_is_useful() {
        let err = MbError::MissingUserAgent;
        let s = format!("{err}");
        assert!(s.contains("musicbrainz_user_agent"));
        assert!(s.contains("config"));
    }
}
