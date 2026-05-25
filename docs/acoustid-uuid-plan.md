# Phase D — AcoustID UUID lookup at rip time

**Status:** Deferred. Phase C (`ACOUSTID_FINGERPRINT` tag write) covers ~95% of
the Picard interop value on its own — Picard can read the fingerprint and do
its own UUID lookup if a user ever wants the round trip closed. Revisit when
there is a concrete need that the fingerprint tag alone can't satisfy.

Companion to Phases A-C, which extend `tag_ripped_file` to write Picard-equivalent
release / track / fingerprint tags during CD rip.

## Why MusicBrainz can't give us the UUID

We verified empirically against a local MB mirror (Pink Floyd's *Dark Side of
the Moon*, `b84ee12a-09ef-421b-82de-0441a926375b`):

```
GET /ws/2/release/b84ee12a-…?inc=recordings+url-rels&fmt=json
→ recording.relations == []   (for every track)
```

AcoustID is a separate MetaBrainz database. MB stores fingerprint → MBID *queries*
in AcoustID's database, not AcoustID UUIDs as recording relationships. There is
no MB endpoint that returns an AcoustID UUID for a given MBID. The only path to
a UUID is hitting `api.acoustid.org` directly — either submit a fingerprint or
look up by fingerprint.

## Implementation sketch

### Config

`~/.config/zytunes/config.toml`:

```toml
acoustid_api_key = "..."   # required; no key → Phase D silently skips
```

Add `acoustid_api_key: Option<String>` to `Config` in `src/tui/config.rs`. Env
fallback `ACOUSTID_API_KEY` for ergonomics — read inside the client constructor.

### New module — `src/acoustid.rs`

Model byte-for-byte on `src/musicbrainz.rs`:

- `AcoustIdClient` struct with `client: ureq::Agent`, `api_key: String`,
  `rate_limit: RateLimiter` (3 req/s — AcoustID's documented limit, matches MB's
  pattern).
- `pub fn lookup_by_fingerprint(&self, fp: &str, duration_secs: u32) -> Result<Option<String>, AcoustIdError>`
  - POST `https://api.acoustid.org/v2/lookup`
  - body: `client=<api_key>&fingerprint=<fp>&duration=<secs>&meta=recordingids`
  - returns the first result's `id` field (UUID) or `None` if no match.
- `AcoustIdError` enum mirroring `MbError` shape.
- One unit test per JSON shape (no match, single match, multi match) using
  hand-rolled response fixtures. No network in tests.

Re-use the existing `ureq` dependency — already pulled in by `musicbrainz.rs`.
**No new runtime deps.**

### Wire into the rip pipeline

In `src/tui/background.rs::run_single_track_rip`:

```rust
// After Phase C's tag_ripped_fingerprint, before std::fs::rename:
if let (Some(api_key), Some(fp)) = (&cfg.acoustid_api_key, &fingerprint) {
    if let Ok(Some(uuid)) = acoustid.lookup_by_fingerprint(fp, duration_secs) {
        let _ = tag_acoustid_uuid(&temp_path, &uuid);
    }
}
```

Where `tag_acoustid_uuid` writes `ItemKey::Unknown("ACOUSTID_ID")` (Picard's
canonical name; matches what dirlib's scanner would need to read it back).

Cancellation: check `cancel.load()` before the HTTP call. The call itself is
~1-2 s — not worth threading a cancel token through `ureq` mid-request.

### What gets passed through

`RipAndImportRequest` already carries `compute_acoustid_fingerprint: bool` per
Phase C. Add:

- `acoustid_api_key: Option<String>` (cloned from config at request-build time;
  keeps the worker config-free per the existing doctrine in
  `src/tui/background.rs`).

A request with `Some(api_key)` and a successfully-computed fingerprint triggers
the lookup; everything else short-circuits.

### Library reads

The library `Track` struct in `src/library.rs` doesn't currently have an
AcoustID UUID field. If Phase D ships, add:

```rust
pub acoustid_id: Option<String>,
```

And in `src/dirlib.rs` near the existing `read_embedded_fingerprint` call (line
~415), read `ItemKey::Unknown("ACOUSTID_ID")` the same way. Bump
`CACHE_SCHEMA_VERSION` in `src/cache.rs` — the field addition would silently
None on stale cache entries otherwise (existing convention; see
`src/cache.rs` header comment).

## Costs / risks

- **Network round-trip per ripped track.** AcoustID has no local-mirror option.
  A 12-track CD pays an extra ~12-20 s wallclock on top of rip + fingerprint
  compute. Tolerable, but call it out in the rip-status UI.
- **API key friction.** First time the codebase needs a user-supplied secret.
  No existing pattern to copy; design `acoustid_api_key` to fail-open (missing
  key = skip phase D silently, log to the sync log, never block the rip).
- **AcoustID outage = no UUIDs.** Fail-open: log a warning, continue. Don't
  hold up the rename.
- **Submission, not just lookup.** AcoustID accepts fingerprint submissions —
  if a track has no match in their DB, a user could submit. Out of scope for
  the initial Phase D. Lookup-only.

## Tests

Mirror the Phase A test patterns:

- `acoustid_parses_single_match` — hand-rolled JSON, assert UUID extraction.
- `acoustid_parses_no_match` — empty `results: []`, assert `Ok(None)`.
- `acoustid_parses_multi_match_returns_first` — assert deterministic pick.
- `acoustid_handles_http_error` — assert `AcoustIdError::Http`.
- `acoustid_handles_missing_api_key` — assert `AcoustIdError::MissingApiKey`.

And one integration-style test in `src/cd/metadata.rs`:

- `tag_acoustid_uuid_writes_unknown_id_key` — write sentinel UUID via
  `tag_acoustid_uuid`, read back via lofty, assert presence.

## Pickup checklist

When resuming:

1. Re-verify the MB-doesn't-have-it claim against a current MB release (the
   schema does evolve; if MB ever adds an AcoustID URL relation type, the
   plan changes).
2. Get an AcoustID API key from `https://acoustid.org/api-key` (free, requires
   account).
3. Confirm `ureq` is still the HTTP client in use; otherwise update `acoustid.rs`
   to match.
4. Phase A/B/C should be merged by then — read the resulting `tag_ripped_file`
   shape before adding the UUID write so it slots into the same ordering.

## Commit

```
feat(cd): look up AcoustID UUID after fingerprint
```
