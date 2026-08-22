//! `zytunes-serve` — LAN HTTP API for the local music library.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use zytunes::art_cache::ArtCache;
use zytunes::dirlib::{DirectoryLibrary, ScanOptions};
use zytunes::library::MusicLibrary;
use zytunes::resolve_music_dir;
use zytunes_stream::{build_router, AppState};

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("zytunes-serve: {e}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    let opts = ServeOpts::parse(&args)?;

    let music_dir =
        resolve_serve_music_dir(opts.music_dir_cli.as_deref(), opts.music_dir.as_deref())?;
    eprintln!("Scanning library at {music_dir}...");

    let scan_opts = ScanOptions {
        fingerprint: false,
        ..ScanOptions::default()
    };
    let lib = DirectoryLibrary::scan_with_options(&music_dir, scan_opts, |_| {})?;
    let track_count = lib.track_count();
    let music_root = PathBuf::from(
        lib.music_folder()
            .ok_or_else(|| "library has no music folder".to_string())?,
    );

    let art_cache = ArtCache::default_location();
    let state = AppState::new(Arc::new(lib), music_root, art_cache, opts.token.clone())
        .with_plays(
            zytunes::local_plays::LocalPlays::load(),
            zytunes::local_plays::default_save_path(),
        )
        .with_listen_log(zytunes::listen_log::ListenLog::load());

    if let Some(err) = insecure_bind_error(&opts.bind, opts.token.is_some(), opts.allow_insecure) {
        return Err(err);
    }

    let bind: SocketAddr = format!("{}:{}", opts.bind, opts.port)
        .parse()
        .map_err(|e| format!("invalid bind address: {e}"))?;

    if opts.token.is_some() && !is_loopback_host(&opts.bind) {
        eprintln!(
            "warning: serving over HTTP on {bind} — Authorization Bearer tokens \
             are visible on the path; terminate TLS (Caddy/nginx) on untrusted networks"
        );
    }
    if opts.allow_insecure && opts.token.is_none() {
        eprintln!(
            "warning: listening on {bind} with no auth token (--allow-insecure) — \
             anyone who can reach this address can browse, download, trigger stem \
             splits, and write play history (loopback is reachable by other users \
             on this host)"
        );
    }

    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(|e| format!("bind {bind}: {e}"))?;
    eprintln!("Serving {track_count} tracks on http://{bind}");

    axum::serve(listener, app)
        .await
        .map_err(|e| format!("server error: {e}"))?;
    Ok(())
}

/// `true` for hosts that only accept connections from this machine.
fn is_loopback_host(bind: &str) -> bool {
    bind == "127.0.0.1" || bind == "localhost" || bind == "::1"
}

/// `Some(message)` when the requested bind has neither a non-empty token
/// nor `--allow-insecure`. Loopback is not exempt: other local users on a
/// shared host can connect to `127.0.0.1` just as a LAN client can connect
/// to `0.0.0.0`.
fn insecure_bind_error(bind: &str, has_token: bool, allow_insecure: bool) -> Option<String> {
    if has_token || allow_insecure {
        return None;
    }
    Some(format!(
        "refusing to bind {bind} without an auth token — set [stream] token / --token, \
         or pass --allow-insecure to accept unauthenticated access (loopback is still \
         reachable by other users on this host)"
    ))
}

/// Treat missing, empty, and whitespace-only secrets as "no token" so
/// `token = ""` cannot satisfy the bind guard while authenticating as
/// `Authorization: Bearer `.
fn nonempty_secret(token: Option<String>) -> Option<String> {
    token
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// `--music-dir` wins over `ZYTUNES_MUSIC_DIR` and config.toml. Without the
/// flag, fall through to the shared CLI/TUI resolver (env, then config).
fn resolve_serve_music_dir(cli: Option<&str>, config: Option<&str>) -> Result<String, String> {
    if let Some(dir) = cli.map(str::trim).filter(|s| !s.is_empty()) {
        if Path::new(dir).is_dir() {
            return Ok(dir.to_string());
        }
        return Err(format!("--music-dir is not a directory: {dir}"));
    }
    resolve_music_dir(config)
}

#[derive(Debug, Default)]
struct ServeOpts {
    bind: String,
    port: u16,
    token: Option<String>,
    /// `music_dir` from config.toml (env is applied inside `resolve_music_dir`
    /// when no `--music-dir` flag is present).
    music_dir: Option<String>,
    /// Explicit `--music-dir`. Wins over `ZYTUNES_MUSIC_DIR` and config.
    music_dir_cli: Option<String>,
    allow_insecure: bool,
}

impl ServeOpts {
    fn parse(args: &[String]) -> Result<Self, String> {
        let cfg = load_file_config();
        let env = load_stream_env();
        let mut opts = ServeOpts {
            bind: env
                .bind
                .or(cfg.stream.bind)
                .unwrap_or_else(|| "0.0.0.0".to_string()),
            port: env.port.or(cfg.stream.port).unwrap_or(9847),
            token: env.token.or(cfg.stream.token),
            // Config only here; `--music-dir` is tracked separately so it can
            // beat `ZYTUNES_MUSIC_DIR` (which `resolve_music_dir` prefers).
            music_dir: cfg.music_dir,
            music_dir_cli: None,
            allow_insecure: env
                .allow_insecure
                .or(cfg.stream.allow_insecure)
                .unwrap_or(false),
        };

        let mut i = 1;
        while i < args.len() {
            match args[i].as_str() {
                "--bind" => {
                    i += 1;
                    opts.bind = args.get(i).ok_or("--bind requires a value")?.clone();
                }
                "--port" => {
                    i += 1;
                    opts.port = args
                        .get(i)
                        .ok_or("--port requires a value")?
                        .parse()
                        .map_err(|e| format!("invalid --port: {e}"))?;
                }
                "--token" => {
                    i += 1;
                    opts.token = Some(args.get(i).ok_or("--token requires a value")?.clone());
                }
                "--music-dir" => {
                    i += 1;
                    opts.music_dir_cli =
                        Some(args.get(i).ok_or("--music-dir requires a value")?.clone());
                }
                "--allow-insecure" => {
                    opts.allow_insecure = true;
                }
                "-h" | "--help" => {
                    print_help();
                    std::process::exit(0);
                }
                other => return Err(format!("unknown argument: {other}")),
            }
            i += 1;
        }
        opts.token = nonempty_secret(opts.token);
        Ok(opts)
    }
}

#[derive(Debug, Default)]
struct StreamConfig {
    bind: Option<String>,
    port: Option<u16>,
    token: Option<String>,
    allow_insecure: Option<bool>,
}

/// The subset of `~/.config/zytunes/config.toml` the server reads:
/// the top-level `music_dir` shared with the TUI/CLI plus `[stream]`.
#[derive(Debug, Default)]
struct FileConfig {
    music_dir: Option<String>,
    stream: StreamConfig,
}

fn load_file_config() -> FileConfig {
    let Ok(home) = std::env::var("HOME") else {
        return FileConfig::default();
    };
    let path = std::path::Path::new(&home)
        .join(".config")
        .join("zytunes")
        .join("config.toml");
    let Ok(contents) = std::fs::read_to_string(path) else {
        return FileConfig::default();
    };
    parse_config(&contents)
}

fn parse_config(contents: &str) -> FileConfig {
    let Ok(table) = contents.parse::<toml::Table>() else {
        return FileConfig::default();
    };
    let music_dir = table
        .get("music_dir")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let stream = table
        .get("stream")
        .and_then(|v| v.as_table())
        .map(|stream| StreamConfig {
            bind: stream
                .get("bind")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            port: stream
                .get("port")
                .and_then(|v| v.as_integer())
                .and_then(|n| u16::try_from(n).ok()),
            token: nonempty_secret(
                stream
                    .get("token")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
            ),
            allow_insecure: stream.get("allow_insecure").and_then(|v| v.as_bool()),
        })
        .unwrap_or_default();
    FileConfig { music_dir, stream }
}

/// `[stream]` config as environment variables — the natural way to
/// configure a container without bind-mounting a config.toml. Takes
/// precedence over config.toml but not over CLI flags.
#[derive(Debug, Default)]
struct StreamEnv {
    bind: Option<String>,
    port: Option<u16>,
    token: Option<String>,
    allow_insecure: Option<bool>,
}

fn load_stream_env() -> StreamEnv {
    StreamEnv {
        bind: std::env::var("ZYTUNES_STREAM_BIND").ok(),
        port: std::env::var("ZYTUNES_STREAM_PORT")
            .ok()
            .and_then(|v| v.parse().ok()),
        token: nonempty_secret(std::env::var("ZYTUNES_STREAM_TOKEN").ok()),
        allow_insecure: std::env::var("ZYTUNES_STREAM_ALLOW_INSECURE")
            .ok()
            .map(|v| parse_bool_env(&v)),
    }
}

fn parse_bool_env(v: &str) -> bool {
    matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes")
}

fn print_help() {
    println!("zytunes-serve — stream the local zytunes library over HTTP\n");
    println!("Usage: zytunes-serve [options]\n");
    println!("Options:");
    println!("  --bind <addr>       Listen address (default: 0.0.0.0 or [stream] bind)");
    println!("  --port <port>       Listen port (default: 9847 or [stream] port)");
    println!("  --token <secret>    Require Authorization: Bearer <secret>");
    println!("  --music-dir <path>  Override music library path (wins over ZYTUNES_MUSIC_DIR)");
    println!("  --allow-insecure    Allow binding with no token (any address)");
    println!("  -h, --help          Show this help");
    println!("\nA non-empty token is required unless you pass --allow-insecure.");
    println!("Loopback is not a multi-user boundary — other accounts on this host");
    println!("can still connect. Serving is HTTP; put TLS in front on untrusted networks.");
    println!("\nConfig (~/.config/zytunes/config.toml):");
    println!("  [stream]");
    println!("  bind = \"0.0.0.0\"");
    println!("  port = 9847");
    println!("  token = \"optional-shared-secret\"");
    println!("  allow_insecure = false");
    println!("\nOr environment variables (checked before config.toml, e.g. for Docker):");
    println!("  ZYTUNES_STREAM_BIND, ZYTUNES_STREAM_PORT, ZYTUNES_STREAM_TOKEN,");
    println!("  ZYTUNES_STREAM_ALLOW_INSECURE=true, and ZYTUNES_MUSIC_DIR for the library path.");
    println!("\nStems use the TUI [stems] recipe and ~/.cache/zytunes/stems.");
    println!("The server will not install demucs — press M in zytunes-tui once.");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bind_without_token_or_opt_in_is_refused() {
        assert!(insecure_bind_error("127.0.0.1", false, false).is_some());
        assert!(insecure_bind_error("localhost", false, false).is_some());
        assert!(insecure_bind_error("::1", false, false).is_some());
        assert!(insecure_bind_error("0.0.0.0", false, false).is_some());
    }

    #[test]
    fn loopback_with_token_or_opt_in_is_allowed() {
        assert!(insecure_bind_error("127.0.0.1", true, false).is_none());
        assert!(insecure_bind_error("127.0.0.1", false, true).is_none());
    }

    #[test]
    fn lan_bind_without_token_or_opt_in_is_refused() {
        assert!(insecure_bind_error("0.0.0.0", false, false).is_some());
    }

    #[test]
    fn nonempty_secret_drops_blank_values() {
        assert_eq!(nonempty_secret(None), None);
        assert_eq!(nonempty_secret(Some(String::new())), None);
        assert_eq!(nonempty_secret(Some("   ".into())), None);
        assert_eq!(
            nonempty_secret(Some(" secret ".into())).as_deref(),
            Some("secret")
        );
    }

    #[test]
    fn empty_config_token_does_not_count_as_set() {
        let cfg = parse_config("[stream]\ntoken = \"\"\n");
        assert_eq!(cfg.stream.token, None);
        let cfg = parse_config("[stream]\ntoken = \"  \"\n");
        assert_eq!(cfg.stream.token, None);
    }

    #[test]
    fn lan_bind_with_token_is_allowed() {
        assert!(insecure_bind_error("0.0.0.0", true, false).is_none());
    }

    #[test]
    fn lan_bind_with_explicit_opt_in_is_allowed() {
        assert!(insecure_bind_error("0.0.0.0", false, true).is_none());
    }

    #[test]
    fn config_music_dir_is_read_from_top_level() {
        let cfg = parse_config("music_dir = \"/Volumes/Library/Music\"\n");
        assert_eq!(cfg.music_dir.as_deref(), Some("/Volumes/Library/Music"));
    }

    #[test]
    fn config_music_dir_coexists_with_stream_table() {
        let cfg = parse_config("music_dir = \"/m\"\n\n[stream]\nport = 1234\ntoken = \"s\"\n");
        assert_eq!(cfg.music_dir.as_deref(), Some("/m"));
        assert_eq!(cfg.stream.port, Some(1234));
        assert_eq!(cfg.stream.token.as_deref(), Some("s"));
    }

    #[test]
    fn config_without_music_dir_or_stream_is_empty() {
        let cfg = parse_config("theme = \"Gruvbox Dark\"\n");
        assert_eq!(cfg.music_dir, None);
        assert_eq!(cfg.stream.port, None);
    }

    #[test]
    fn cli_music_dir_wins_over_config_path() {
        let dir = std::env::temp_dir();
        let dir_s = dir.to_str().expect("temp dir is unicode");
        assert_eq!(
            resolve_serve_music_dir(Some(dir_s), Some("/this/does/not/exist")).unwrap(),
            dir_s
        );
    }

    #[test]
    fn missing_cli_music_dir_is_an_error() {
        let err =
            resolve_serve_music_dir(Some("/definitely-not-a-zytunes-music-dir"), None).unwrap_err();
        assert!(err.contains("--music-dir is not a directory"), "{err}");
    }

    #[test]
    fn bool_env_accepts_common_truthy_spellings() {
        for v in ["1", "true", "TRUE", "yes", " Yes "] {
            assert!(parse_bool_env(v), "{v:?} should be truthy");
        }
        for v in ["0", "false", "no", "", "on"] {
            assert!(!parse_bool_env(v), "{v:?} should be falsy");
        }
    }
}
