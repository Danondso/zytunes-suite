# zytunes stream API

LAN HTTP API for browsing and streaming a local zytunes music library.
Designed for a future mobile client (local Spotify-style browse/search/play).
Stem splitting runs **on the server** (same cache and recipe as the TUI) so
phones never have to run demucs. The Flutter client downloads the resulting
FLACs and mixes them from local files — N concurrent HTTP streams drift.

## Run

```bash
zytunes-serve [--bind 0.0.0.0] [--port 9847] --token SECRET [--music-dir PATH]
```

Music directory resolution: `--music-dir`, then `ZYTUNES_MUSIC_DIR`, then
`music_dir` in `~/.config/zytunes/config.toml`. The flag wins even when the
env var is set (containers still use the env var when no flag is passed).

Optional config:

```toml
[stream]
bind = "0.0.0.0"
port = 9847
token = "shared-secret"
```

Default bind is `0.0.0.0:9847`, and the server **refuses to start** without a
non-empty token. Empty or whitespace-only `[stream] token` values are treated as
unset (they must not satisfy the bind guard). Loopback
(`127.0.0.1`/`localhost`/`::1`) is **not** exempt — other local users on a
shared host can connect. There is no `--allow-insecure` escape hatch;
leftover copies of that flag, `[stream] allow_insecure = true`, or
`ZYTUNES_STREAM_ALLOW_INSECURE` fail the process. This exists because
`POST /tracks/{id}/stems` alone lets an unauthenticated client trigger
unbounded CPU-heavy separation jobs, and `POST /tracks/{id}/play` can grow
on-disk play history.

The server speaks **HTTP**. When a token is configured, clients send it as:

```
Authorization: Bearer <token>
```

On an untrusted LAN or Wi-Fi, terminate TLS in front (Caddy/nginx). There
is no built-in HTTPS.

The same settings are also readable from the environment (checked before
`config.toml`, so a container needs no mounted config file):

| Variable | Equivalent |
|----------|------------|
| `ZYTUNES_MUSIC_DIR` | `--music-dir` (already the CLI/TUI convention) |
| `ZYTUNES_STREAM_BIND` | `--bind` / `[stream] bind` |
| `ZYTUNES_STREAM_PORT` | `--port` / `[stream] port` |
| `ZYTUNES_STREAM_TOKEN` | `--token` / `[stream] token` |

CLI flags still win over environment variables, which win over `config.toml`.

## Docker

```bash
cp .env.example .env   # set ZYTUNES_MUSIC_DIR and ZYTUNES_STREAM_TOKEN
docker compose up --build
```

`docker-compose.yml` (repo root) builds `zytunes-stream/Dockerfile` and
**bind-mounts** `ZYTUNES_MUSIC_DIR` read-only into the container at `/music`
— your library is read directly from the host path, nothing is copied into
a volume. A separate named volume (`zytunes-cache`) persists the
library/art/stem caches under `~/.cache/zytunes` across restarts so a
container restart doesn't force a full library rescan.

`docker compose up` refuses to start without `ZYTUNES_STREAM_TOKEN` set (in
`.env` or the environment), matching the binary's own refusal to start
without a non-empty token — `docker-compose.yml`'s default bind is
`0.0.0.0` inside the container network.

Without Compose:

```bash
docker build -f zytunes-stream/Dockerfile -t zytunes-serve .
docker run -d --name zytunes-serve -p 9847:9847 \
  -v /path/to/your/music:/music:ro \
  -e ZYTUNES_MUSIC_DIR=/music \
  -e ZYTUNES_STREAM_TOKEN=changeme \
  zytunes-serve
```

## Endpoints

All JSON uses snake_case. Track IDs are the existing path-hash `u64` values
from the directory library, **serialized as decimal strings** so JavaScript
and Dart clients do not overflow (`"id": "42"`). Stable while the file path
is unchanged. URL paths still use the decimal digits: `/tracks/42/stream`.

| Method | Path | Notes |
|--------|------|-------|
| `GET` | `/health` | `{ "ok": true }` |
| `GET` | `/artists` | Sorted artist name strings |
| `GET` | `/albums?artist=` | `[{ "artist", "album", "track_count", "year?", "art_url?" }, …]` (optional artist filter) |
| `GET` | `/tracks?artist=&album=` | `TrackSummary` list |
| `GET` | `/tracks/{id}` | `TrackDetail` (metadata + URLs + play_count when > 0) |
| `GET` | `/search?q=` | `{ "artists", "albums", "tracks" }` — artists and albums whose names match, then tracks ranked title → artist → album |
| `GET` | `/tracks/{id}/stream` | Playback bytes; supports `Range`. Does **not** increment play count. |
| `GET` | `/tracks/{id}/file` | **Original** file, no transcode; `Content-Disposition: attachment` |
| `GET` | `/tracks/{id}/art` | JPEG bytes, or `404` |
| `POST` | `/tracks/{id}/play` | Record one listen (same sidecar as the TUI). Same-track repeat within 30s is a no-op; more than 60 recorded plays per minute is `429`. `404` if unknown. |
| `GET` | `/tracks/{id}/stems` | Stem job status + layout + per-stem URLs |
| `POST` | `/tracks/{id}/stems` | Start a split if missing; no-op if already ready |
| `DELETE` | `/tracks/{id}/stems` | Cancel an in-flight split for this id |
| `GET` | `/tracks/{id}/stems/{kind}` | One stem FLAC; supports `Range` |

Unknown ids and paths outside the library root return `404`. Clients never
send filesystem paths.

### AlbumPair

```json
{
  "artist": "Radiohead",
  "album": "OK Computer",
  "track_count": 12,
  "year": 1997,
  "art_url": "/tracks/42/art"
}
```

`year` and `art_url` are omitted when unknown. `art_url` is the first track
in disc/track order (`GET /tracks/{id}/art`).

### Search

```json
{
  "artists": ["Radiohead"],
  "albums": [
    {
      "artist": "Radiohead",
      "album": "OK Computer",
      "track_count": 12,
      "year": 1997,
      "art_url": "/tracks/42/art"
    }
  ],
  "tracks": [
    {
      "id": "42",
      "name": "Karma Police",
      "artist": "Radiohead",
      "album": "OK Computer"
    }
  ]
}
```

Empty sections are `[]`. Album-title matches also include that album's artist
so a query like `OK Computer` can open the artist page. Track-title-only hits
do not add artist/album rows — clients offer Go to album / Go to artist from
the track overflow menu.

### TrackSummary

```json
{
  "id": "42",
  "name": "Karma Police",
  "artist": "Radiohead",
  "album": "OK Computer",
  "track_number": 1,
  "disc_number": null,
  "duration_ms": 262000,
  "kind": "FLAC"
}
```

### TrackDetail

Summary fields plus optional extended metadata (genre, year, album_artist,
composer, sample_rate, channels, bit_depth, audio_bitrate_kbps,
file_size_bytes, MusicBrainz / ReplayGain fields when present) and:

```json
{
  "stream_url": "/tracks/42/stream",
  "file_url": "/tracks/42/file",
  "art_url": "/tracks/42/art",
  "play_count": 3,
  "last_played_at_ms": 1700000000000
}
```

`play_count` and `last_played_at_ms` are omitted when the track has never been recorded as a play.

### Play count

`GET /stream` does not increment play count — Range seeks, buffering, and retries would inflate it. Clients `POST /tracks/{id}/play` once per listen after the iTunes threshold (50% of duration or 4 minutes, whichever first — `play_threshold_ms` in `zytunes::local_plays`). A second POST for the same track within 30 seconds returns the existing counts without writing (so retries cannot inflate the sidecar). More than 60 recorded plays in a rolling minute returns `429`.

```json
{ "play_count": 1, "last_played_at_ms": 1700000000000 }
```

Writes the same sidecar the TUI uses (`~/.cache/zytunes/local-plays.json`) and appends a completed event to the listen log so the recommender sees LAN plays. Unknown ids are `404`.

### Range (seek)

```
GET /tracks/42/stream
Range: bytes=0-1023
```

Response: `206 Partial Content` with `Accept-Ranges: bytes`,
`Content-Range: bytes 0-1023/<size>`, and the requested slice.
Without `Range`, the full file is returned as `200` with `Accept-Ranges: bytes`.

Content-Type is derived from the file extension (`audio/flac`, `audio/mpeg`,
`audio/mp4`, `audio/wav`, …).

### Stems

The server reuses the TUI stem cache (`[stems] cache_dir`, default
`~/.cache/zytunes/stems`) and the `[stems]` recipe (`demucs` / `hq` /
`sw` / `hq-harmony`). A track already split
in the TUI is ready immediately. The server does **not** auto-provision
Python engines — press `M` once in `zytunes-tui` to install.

`kind` is the filename slug: `vocals`, `drums`, `bass`, `guitar`, `piano`,
`other`, plus `lead` / `backing` for harmony recipes. Unknown kinds and
stems that are not yet in cache return `404`.

```json
{
  "status": "ready",
  "recipe": "demucs",
  "layout": ["vocals", "drums", "bass", "guitar", "piano", "other"],
  "stems": [
    {
      "kind": "vocals",
      "label": "Vocals",
      "short_label": "Voc",
      "url": "/tracks/42/stems/vocals"
    }
  ],
  "engine_available": true
}
```

`status` is `ready`, `missing`, `separating`, or `failed`. `progress`
(0–100) is present while separating. `error` is present on `failed`
(engine not installed, separator crash). JSON is `200` for a known track
even when the job failed; unknown ids are `404`.

`GET /tracks/{id}/file` is still the untranscoded original, for tools
that want to split elsewhere.
