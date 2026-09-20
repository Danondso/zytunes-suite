//! Append-only listen-history log for the sequence-aware recommender.
//!
//! Each TUI play (or skip) appends one JSONL event:
//! ```json
//! {"ts":1700000000000,"id":12345,"completed":true}
//! ```
//!
//! Lives at `~/.cache/zytunes/listen-log.jsonl`. The "cache" location is
//! deliberate: the log is *useful but not user-authored* — losing it costs
//! the user some recommendation quality but no curated state. Worktrees
//! sharing a `~/Music` root share one log (intentionally — same reasoning as
//! `local_plays`).
//!
//! ## Why JSONL
//!
//! Append-only, grep-friendly, ~50 bytes per event. At 100 plays/day for
//! 5 years that's 9 MB — the recommender reads it once at generation time,
//! parses are bounded by library size in practice, and binary encoding
//! would only buy 3× compression at the cost of versioning headaches.
//!
//! ## Sessions
//!
//! Events are grouped into "listening sessions" by time gap. The default
//! gap is 30 minutes — anything longer than that is treated as a new
//! session boundary so the bigram model doesn't learn spurious "morning
//! commute → evening unwind" transitions.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::cache::{default_logger, Logger};

/// Default session gap: events more than this far apart are treated as
/// different sessions. 30 minutes mirrors what most listening-history
/// tools settle on (long enough to forgive a pause for coffee, short
/// enough that morning vs. evening don't blur into one session).
pub const DEFAULT_SESSION_GAP_MS: u64 = 30 * 60 * 1000;

/// One entry in the listen log.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListenEvent {
    /// Unix epoch ms.
    pub ts: u64,
    /// Library track ID (`Track::id`).
    pub id: u64,
    /// `true` when the play crossed the play-count threshold (a real play),
    /// `false` for skips. Drives the bigram model's decision to weight
    /// completed plays more than skipped ones.
    pub completed: bool,
}

/// In-memory view of the listen log. Built by `load_from`; mutated only
/// by appends, which write straight through to disk for crash safety.
#[derive(Clone)]
pub struct ListenLog {
    events: Vec<ListenEvent>,
    /// Path the log persists to. `None` means "in-memory only" (used by
    /// tests + the App when no `HOME` is set).
    save_path: Option<PathBuf>,
    logger: Logger,
}

impl Default for ListenLog {
    fn default() -> Self {
        ListenLog {
            events: Vec::new(),
            save_path: None,
            logger: default_logger(),
        }
    }
}

impl std::fmt::Debug for ListenLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ListenLog")
            .field("events", &self.events)
            .field("save_path", &self.save_path)
            .finish_non_exhaustive()
    }
}

impl ListenLog {
    pub fn new() -> Self {
        ListenLog::default()
    }

    /// Bind the log to a path. After this, `append` writes through.
    pub fn with_save_path(mut self, path: PathBuf) -> Self {
        self.save_path = Some(path);
        self
    }

    /// Override the logger. Defaults to `default_logger()` (stderr).
    /// Pass a channel-routing closure in the TUI so disk errors appear
    /// in the sync log instead of scribbling through the ratatui frame.
    pub fn with_logger(mut self, logger: Logger) -> Self {
        self.logger = logger;
        self
    }

    pub fn events(&self) -> &[ListenEvent] {
        &self.events
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Append one event and persist if a save path is bound.
    ///
    /// Best-effort persistence: the event is added to in-memory state
    /// unconditionally, then a write is attempted. Disk errors go through
    /// the [`Logger`] (stderr in the CLI, the TUI sync log in the TUI)
    /// and the in-memory state stays consistent so the caller's logic is
    /// unaffected. **Trade-off:** events appended in this session that
    /// fail to persist survive until process exit and are gone after a
    /// crash. Treat the log as an info signal, not a source of truth —
    /// the recommender layers this against the live library scan, so
    /// missing rows are tolerated.
    pub fn append(&mut self, event: ListenEvent) {
        self.events.push(event);
        // Borrow rather than clone — `append_to_disk` only needs `&Path`,
        // and this fires on every play/skip in the hot path.
        if let Some(path) = self.save_path.as_deref() {
            Self::append_to_disk(path, &event, &self.logger);
        }
    }

    fn append_to_disk(path: &Path, event: &ListenEvent, log: &Logger) {
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                log(&format!(
                    "zytunes: listen-log: mkdir {} failed: {e}",
                    parent.display()
                ));
                return;
            }
        }
        let line = match serde_json::to_string(event) {
            Ok(s) => s,
            Err(e) => {
                log(&format!("zytunes: listen-log: serialize event failed: {e}"));
                return;
            }
        };
        let mut f = match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            Ok(f) => f,
            Err(e) => {
                log(&format!(
                    "zytunes: listen-log: open {} failed: {e}",
                    path.display()
                ));
                return;
            }
        };
        if let Err(e) = writeln!(f, "{line}") {
            log(&format!(
                "zytunes: listen-log: write {} failed: {e}",
                path.display()
            ));
            return;
        }
        // Best-effort flush so the appended line survives a crash within
        // the (small but real) page-cache window. Errors here are
        // diagnostic-only — the in-memory state is already updated and
        // the recommender treats the log as an info signal.
        if let Err(e) = f.sync_all() {
            log(&format!(
                "zytunes: listen-log: sync_all {} failed: {e}",
                path.display()
            ));
        }
    }

    /// Read the log from the well-known path. Missing or empty files yield
    /// an empty in-memory log.
    pub fn load() -> Self {
        let logger = default_logger();
        match default_save_path() {
            Some(p) => Self::load_from(&p, &logger).with_save_path(p),
            None => ListenLog::default(),
        }
    }

    /// Read from an arbitrary path. Lines that fail to parse are skipped
    /// with a warning — one malformed event must never poison the rest of
    /// the log. Events come back in file order (which equals chronological
    /// order for an append-only log).
    ///
    /// The supplied `log` is stored on the returned instance so subsequent
    /// `append` calls route disk errors through the same sink.
    pub fn load_from(path: &Path, log: &Logger) -> Self {
        let data = match std::fs::read_to_string(path) {
            Ok(d) => d,
            Err(e) => {
                if e.kind() != std::io::ErrorKind::NotFound {
                    log(&format!(
                        "zytunes: listen-log: read {} failed: {e}",
                        path.display()
                    ));
                }
                return ListenLog {
                    events: Vec::new(),
                    save_path: None,
                    logger: log.clone(),
                };
            }
        };
        let mut events = Vec::new();
        let mut bad = 0usize;
        for line in data.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            match serde_json::from_str::<ListenEvent>(trimmed) {
                Ok(e) => events.push(e),
                Err(_) => bad += 1,
            }
        }
        if bad > 0 {
            log(&format!(
                "zytunes: listen-log: skipped {bad} malformed line(s) in {}",
                path.display()
            ));
        }
        ListenLog {
            events,
            save_path: None,
            logger: log.clone(),
        }
    }

    /// Group events into listening sessions, splitting on time gaps larger
    /// than `gap_ms`. Returns slices over the underlying events vec so we
    /// don't allocate per session — the bigram builder iterates pairs.
    pub fn sessions(&self, gap_ms: u64) -> Vec<&[ListenEvent]> {
        if self.events.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut start = 0usize;
        for i in 1..self.events.len() {
            let prev = &self.events[i - 1];
            let cur = &self.events[i];
            // Use `saturating_sub` because clock skew between events
            // (rare but possible during NTP corrections) shouldn't crash
            // the iterator — just don't split a session on it.
            let dt = cur.ts.saturating_sub(prev.ts);
            if dt > gap_ms {
                out.push(&self.events[start..i]);
                start = i;
            }
        }
        out.push(&self.events[start..]);
        out
    }
}

/// Default location of the log: `~/.cache/zytunes/listen-log.jsonl`.
/// Returns `None` when `HOME` is unset.
pub fn default_save_path() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(
        Path::new(&home)
            .join(".cache")
            .join("zytunes")
            .join("listen-log.jsonl"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(stem: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "zytunes-listen-log-test-{}-{}.jsonl",
            std::process::id(),
            stem
        ))
    }

    #[test]
    fn append_grows_in_memory() {
        let mut log = ListenLog::new();
        assert!(log.is_empty());
        log.append(ListenEvent {
            ts: 100,
            id: 1,
            completed: true,
        });
        log.append(ListenEvent {
            ts: 200,
            id: 2,
            completed: false,
        });
        assert_eq!(log.len(), 2);
        assert_eq!(log.events()[0].id, 1);
        assert!(!log.events()[1].completed);
    }

    #[test]
    fn append_persists_to_disk_and_load_round_trips() {
        let path = temp_path("round-trip");
        let _ = std::fs::remove_file(&path);

        let mut log = ListenLog::new().with_save_path(path.clone());
        log.append(ListenEvent {
            ts: 100,
            id: 1,
            completed: true,
        });
        log.append(ListenEvent {
            ts: 200,
            id: 2,
            completed: false,
        });

        let loaded = ListenLog::load_from(&path, &default_logger());
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded.events()[0].id, 1);
        assert_eq!(loaded.events()[1].id, 2);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn append_uses_append_mode_not_overwrite() {
        // If `append` truncated, the second log session would overwrite
        // the first — pin the appending behaviour explicitly.
        let path = temp_path("append-mode");
        let _ = std::fs::remove_file(&path);

        let mut a = ListenLog::new().with_save_path(path.clone());
        a.append(ListenEvent {
            ts: 1,
            id: 100,
            completed: true,
        });
        drop(a);

        let mut b = ListenLog::new().with_save_path(path.clone());
        b.append(ListenEvent {
            ts: 2,
            id: 200,
            completed: true,
        });
        drop(b);

        let loaded = ListenLog::load_from(&path, &default_logger());
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded.events()[0].id, 100);
        assert_eq!(loaded.events()[1].id, 200);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_skips_malformed_lines() {
        let path = temp_path("malformed");
        std::fs::write(
            &path,
            "{\"ts\":1,\"id\":42,\"completed\":true}\n\
             not json\n\
             {\"ts\":2,\"id\":43,\"completed\":false}\n",
        )
        .unwrap();
        let loaded = ListenLog::load_from(&path, &default_logger());
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded.events()[0].id, 42);
        assert_eq!(loaded.events()[1].id, 43);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_missing_file_returns_empty() {
        let path = temp_path("missing");
        let _ = std::fs::remove_file(&path);
        let loaded = ListenLog::load_from(&path, &default_logger());
        assert!(loaded.is_empty());
    }

    #[test]
    fn sessions_split_on_gap() {
        let mut log = ListenLog::new();
        // Three plays in tight succession, then a 90-min gap, then two more.
        log.append(ListenEvent {
            ts: 1_000_000,
            id: 1,
            completed: true,
        });
        log.append(ListenEvent {
            ts: 1_000_000 + 60_000,
            id: 2,
            completed: true,
        });
        log.append(ListenEvent {
            ts: 1_000_000 + 120_000,
            id: 3,
            completed: true,
        });
        log.append(ListenEvent {
            ts: 1_000_000 + 90 * 60_000,
            id: 4,
            completed: true,
        });
        log.append(ListenEvent {
            ts: 1_000_000 + 91 * 60_000,
            id: 5,
            completed: true,
        });

        let sessions = log.sessions(DEFAULT_SESSION_GAP_MS);
        assert_eq!(sessions.len(), 2, "30-min gap → two sessions");
        assert_eq!(sessions[0].len(), 3);
        assert_eq!(sessions[1].len(), 2);
    }

    #[test]
    fn sessions_one_session_when_no_gaps() {
        let mut log = ListenLog::new();
        for i in 0..5 {
            log.append(ListenEvent {
                ts: 1_000_000 + i * 60_000,
                id: i,
                completed: true,
            });
        }
        let sessions = log.sessions(DEFAULT_SESSION_GAP_MS);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].len(), 5);
    }

    #[test]
    fn sessions_empty_log_yields_no_sessions() {
        let log = ListenLog::new();
        assert!(log.sessions(DEFAULT_SESSION_GAP_MS).is_empty());
    }

    #[test]
    fn sessions_handle_clock_skew_without_panicking() {
        // Out-of-order timestamps (rare but possible across NTP correction)
        // mustn't crash the iterator. saturating_sub clamps to 0 → no split.
        let mut log = ListenLog::new();
        log.append(ListenEvent {
            ts: 200,
            id: 1,
            completed: true,
        });
        log.append(ListenEvent {
            ts: 100, // earlier than prior event
            id: 2,
            completed: true,
        });
        let sessions = log.sessions(DEFAULT_SESSION_GAP_MS);
        // One session — backwards delta is treated as 0 (no gap).
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].len(), 2);
    }

    #[test]
    fn append_routes_disk_errors_through_stored_logger() {
        use std::sync::{Arc, Mutex};
        let captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let cap = captured.clone();
        let log: crate::cache::Logger = Arc::new(move |msg: &str| {
            cap.lock().unwrap().push(msg.to_string());
        });

        // /dev/null is a character device; any path beneath it is always
        // unresolvable on Linux, so create_dir_all fails and append_to_disk
        // bails on the mkdir branch (open is never reached — see below for
        // that branch).
        let bad_path = std::path::PathBuf::from("/dev/null/zytunes-test-listen/listen-log.jsonl");
        let mut listen_log = ListenLog::new().with_save_path(bad_path).with_logger(log);
        listen_log.append(ListenEvent {
            ts: 1,
            id: 42,
            completed: true,
        });

        let msgs = captured.lock().unwrap().clone();
        assert!(
            msgs.iter().any(|m| m.contains("mkdir")),
            "append must route mkdir errors through the stored logger, got: {msgs:?}"
        );
        // The event must still be in-memory despite the disk failure.
        assert_eq!(listen_log.len(), 1);

        // Second scenario: a save path that IS an existing directory. Its
        // parent exists so create_dir_all succeeds, and the failure lands
        // on the `open` branch instead (EISDIR).
        captured.lock().unwrap().clear();
        let dir_path = std::env::temp_dir().join(format!(
            "zytunes-listen-log-test-{}-open-branch",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir_path).unwrap();
        let cap2 = captured.clone();
        let log2: crate::cache::Logger = Arc::new(move |msg: &str| {
            cap2.lock().unwrap().push(msg.to_string());
        });
        let mut dir_log = ListenLog::new()
            .with_save_path(dir_path.clone())
            .with_logger(log2);
        dir_log.append(ListenEvent {
            ts: 2,
            id: 43,
            completed: true,
        });
        let msgs = captured.lock().unwrap().clone();
        assert!(
            msgs.iter().any(|m| m.contains("open")),
            "append must route open errors through the stored logger, got: {msgs:?}"
        );
        assert_eq!(dir_log.len(), 1);
        let _ = std::fs::remove_dir(&dir_path);
    }

    #[test]
    fn load_from_routes_errors_through_supplied_logger() {
        use std::sync::{Arc, Mutex};
        let captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let cap = captured.clone();
        let log: crate::cache::Logger = Arc::new(move |msg: &str| {
            cap.lock().unwrap().push(msg.to_string());
        });

        // Malformed lines must be reported through the supplied logger.
        let path = temp_path("logger-malformed");
        std::fs::write(&path, "{\"ts\":1,\"id\":42,\"completed\":true}\nnot json\n").unwrap();
        let loaded = ListenLog::load_from(&path, &log);
        assert_eq!(loaded.len(), 1);
        let msgs = captured.lock().unwrap().clone();
        assert!(
            msgs.iter().any(|m| m.contains("malformed")),
            "load_from must route malformed-line warnings through the supplied logger, got: {msgs:?}"
        );
        let _ = std::fs::remove_file(&path);

        // A read error that isn't NotFound (here: the path is a directory)
        // must also be reported, not silently swallowed or sent to stderr.
        captured.lock().unwrap().clear();
        let dir_path = std::env::temp_dir().join(format!(
            "zytunes-listen-log-test-{}-load-dir",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir_path).unwrap();
        let loaded = ListenLog::load_from(&dir_path, &log);
        assert!(loaded.is_empty());
        let msgs = captured.lock().unwrap().clone();
        assert!(
            msgs.iter().any(|m| m.contains("read")),
            "load_from must route read errors through the supplied logger, got: {msgs:?}"
        );
        let _ = std::fs::remove_dir(&dir_path);
    }
}
