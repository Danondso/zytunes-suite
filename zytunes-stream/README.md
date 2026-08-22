# zytunes-stream

LAN HTTP server for a local zytunes music library. The crate is `zytunes-stream`; the binary is **`zytunes-serve`**.

It scans the same directory library as the CLI/TUI, then serves browse, search, original-file streaming with HTTP `Range`, album art, on-demand stem splits from the TUI stem cache, and client-reported play counts into the same sidecar the TUI uses. Phones never run demucs — they download the resulting FLACs.

The request/response contract lives in [`docs/stream-api.md`](../docs/stream-api.md).

## Run

From the workspace root:

```bash
cargo run -p zytunes-stream -- --token SECRET --music-dir /path/to/Music
```

Release install (`./install.sh`) puts `zytunes-serve` next to `zytunes` and `zytunes-tui` in `/usr/local/bin`:

```bash
zytunes-serve [--bind 0.0.0.0] [--port 9847] [--token SECRET] [--music-dir PATH] [--allow-insecure]
```

Music directory resolution matches the rest of zytunes: `--music-dir`, then `ZYTUNES_MUSIC_DIR`, then `music_dir` in `~/.config/zytunes/config.toml`.

Default listen address is `0.0.0.0:9847`. The process **refuses to start** without a non-empty token unless you pass `--allow-insecure`. Empty or whitespace `[stream] token` values count as unset. Loopback is not exempt: other users on a shared host can still connect to `127.0.0.1`. `POST /tracks/{id}/stems` can kick off unbounded CPU-heavy jobs, so an open bind is not a safe default.

When a token is set, every request needs:

```
Authorization: Bearer <token>
```

The server speaks **HTTP, not HTTPS**. Bearer tokens are visible to anyone on the path; terminate TLS (Caddy, nginx, or a similar reverse proxy) on untrusted networks.

## Config

Flags win over environment variables, which win over `config.toml`.

```toml
# ~/.config/zytunes/config.toml
[stream]
bind = "0.0.0.0"
port = 9847
token = "optional-shared-secret"
allow_insecure = false
```

| Variable | Equivalent |
|----------|------------|
| `ZYTUNES_MUSIC_DIR` | `--music-dir` |
| `ZYTUNES_STREAM_BIND` | `--bind` / `[stream] bind` |
| `ZYTUNES_STREAM_PORT` | `--port` / `[stream] port` |
| `ZYTUNES_STREAM_TOKEN` | `--token` / `[stream] token` |
| `ZYTUNES_STREAM_ALLOW_INSECURE` | `--allow-insecure` (`true` / `1` / `yes`) |

## Stems

The server reuses `[stems]` from the same config file (`recipe`, cache dir, engine path) and writes to `~/.cache/zytunes/stems`. A track already split in `zytunes-tui` (`M`) is a cache hit.

It does **not** install Python engines. Press `M` once in the TUI to provision demucs / audio-separator.

## API

JSON is snake_case. Track ids are directory-library path hashes, serialized as **decimal strings** so Dart/JS keep the full `u64`. URL paths still use the digits: `/tracks/42/stream`.

| Method | Path | Notes |
|--------|------|-------|
| `GET` | `/health` | `{ "ok": true }` |
| `GET` | `/artists` | Sorted artist names |
| `GET` | `/albums?artist=` | Album list; optional artist filter |
| `GET` | `/tracks?artist=&album=` | Track summaries |
| `GET` | `/tracks/{id}` | Detail + stream/file/art URLs; `play_count` when > 0 |
| `GET` | `/search?q=` | Artists, albums, and ranked tracks |
| `GET` | `/tracks/{id}/stream` | Playback bytes; `Range` → `206`. Does not increment play count. |
| `GET` | `/tracks/{id}/file` | Original file, `Content-Disposition: attachment` |
| `GET` | `/tracks/{id}/art` | JPEG, or `404` |
| `POST` | `/tracks/{id}/play` | Record one listen (shared TUI sidecar). Same-track repeat within 30s is a no-op; 60 plays/min → `429`. |
| `GET` | `/tracks/{id}/stems` | Job status, layout, per-stem URLs |
| `POST` | `/tracks/{id}/stems` | Start a split if missing |
| `DELETE` | `/tracks/{id}/stems` | Cancel an in-flight split |
| `GET` | `/tracks/{id}/stems/{kind}` | One stem FLAC; `Range` supported |

Unknown ids and paths outside the library root are `404`. Clients never send filesystem paths. Full payload shapes: [`docs/stream-api.md`](../docs/stream-api.md).

## Docker

Build context is the **repo root** (this crate path-depends on `zytunes`).

```bash
cp .env.example .env   # ZYTUNES_MUSIC_DIR + ZYTUNES_STREAM_TOKEN
docker compose up --build
```

[`docker-compose.yml`](../docker-compose.yml) bind-mounts the host music dir read-only **at its host path** (stem-cache entries are keyed by a hash of the absolute track path, so matching the host path is what makes TUI-made splits cache hits), keeps library/art caches in a named volume, and mounts the host's stem cache and `~/.config/zytunes` so stems and the `[stems]` recipe are shared with the TUI. If your config overrides `[stems] cache_dir`, set `ZYTUNES_STEMS_DIR` in `.env` to the same path (it defaults to `~/.cache/zytunes/stems`). The image ships no Python engine — split (or album-pre-warm) with `M` in `zytunes-tui` and the server picks it up. Compose refuses to start without `ZYTUNES_STREAM_TOKEN`. On a network you trust, drop the token and set `ZYTUNES_STREAM_ALLOW_INSECURE=true` instead.

Without Compose:

```bash
docker build -f zytunes-stream/Dockerfile -t zytunes-serve .
docker run -d --name zytunes-serve -p 9847:9847 \
  -v /path/to/your/music:/path/to/your/music:ro \
  -e ZYTUNES_MUSIC_DIR=/path/to/your/music \
  -e ZYTUNES_STREAM_TOKEN=changeme \
  zytunes-serve
```

## Develop

```bash
cargo test -p zytunes-stream
```

`src/main.rs` is the binary (scan, bind, config). `src/lib.rs` is the Axum router used by that binary and by `tests/api.rs`.
