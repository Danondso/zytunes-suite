//! Stem separation via `demucs` shell-out, plus the on-disk stem cache.
//!
//! Separation is an offline, cached, background step — the usable models
//! (Demucs family) run at or below real time on CPU, so nothing here is
//! invoked on the audio path. The TUI's background worker calls
//! [`cached_stems`] first and only runs [`StemSeparator::separate`] on a
//! miss; the resulting six FLACs land in the cache via [`store_stems`].
//!
//! Shelling out mirrors the ffmpeg precedent (video-sync, CD rip): the
//! engine is an optional external tool, checked at use time with a
//! friendly error. `DemucsCli` is the only separator today; the trait
//! exists so an in-process backend can slot in later without touching
//! the worker or the cache. See `docs/stem-splitting-plan.md`.

pub mod provision;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::cache::{FileFingerprint, Logger};

/// Number of stems produced by the 6-source Demucs models (`htdemucs_6s`).
pub const NUM_STEMS: usize = 6;

/// File extension the cache stores stems as. `--flac` keeps them lossless
/// at roughly half the disk of WAV.
pub const STEM_EXT: &str = "flac";

/// One of the six sources `htdemucs_6s` separates a track into.
///
/// Discriminant order is the UI order (keys `1`–`6`) and the index into
/// [`StemSet::paths`]; [`StemKind::file_stem`] matches the output
/// filenames demucs writes, so the two must stay in lockstep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StemKind {
    Vocals,
    Drums,
    Bass,
    Guitar,
    Piano,
    Other,
}

impl StemKind {
    pub const ALL: [StemKind; NUM_STEMS] = [
        StemKind::Vocals,
        StemKind::Drums,
        StemKind::Bass,
        StemKind::Guitar,
        StemKind::Piano,
        StemKind::Other,
    ];

    /// Index into [`StemSet::paths`] / the gains array.
    pub fn index(self) -> usize {
        self as usize
    }

    /// The filename (sans extension) demucs writes this stem as.
    pub fn file_stem(self) -> &'static str {
        match self {
            StemKind::Vocals => "vocals",
            StemKind::Drums => "drums",
            StemKind::Bass => "bass",
            StemKind::Guitar => "guitar",
            StemKind::Piano => "piano",
            StemKind::Other => "other",
        }
    }

    /// Full label for help text and the wide stem strip.
    pub fn label(self) -> &'static str {
        match self {
            StemKind::Vocals => "Vocals",
            StemKind::Drums => "Drums",
            StemKind::Bass => "Bass",
            StemKind::Guitar => "Guitar",
            StemKind::Piano => "Piano",
            StemKind::Other => "Other",
        }
    }

    /// Three-character label for the now-playing strip.
    pub fn short_label(self) -> &'static str {
        match self {
            StemKind::Vocals => "Voc",
            StemKind::Drums => "Drm",
            StemKind::Bass => "Bas",
            StemKind::Guitar => "Gtr",
            StemKind::Piano => "Pno",
            StemKind::Other => "Oth",
        }
    }
}

/// The six stem files for one track, indexed by [`StemKind::index`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StemSet {
    pub paths: [PathBuf; NUM_STEMS],
}

impl StemSet {
    /// Assemble the canonical `dir/{vocals,drums,…}.{ext}` layout — both
    /// demucs' output directory and our cache entries use this shape.
    pub fn from_dir(dir: &Path, ext: &str) -> StemSet {
        StemSet {
            paths: StemKind::ALL.map(|k| dir.join(format!("{}.{ext}", k.file_stem()))),
        }
    }

    /// True when every stem file exists on disk.
    pub fn all_exist(&self) -> bool {
        self.paths.iter().all(|p| p.is_file())
    }
}

/// Shared per-stem gain targets: `f32` bits stored in `AtomicU32`, indexed
/// by [`StemKind::index`]. The app owns one clone and the audio thread's
/// mixer another; toggling a stem is a lock-free store the mixer picks up
/// on its next frame — no channel round-trip.
pub type StemGains = Arc<[std::sync::atomic::AtomicU32; NUM_STEMS]>;

/// Fresh gains with each stem at `1.0` (enabled) or `0.0` (muted).
pub fn new_stem_gains(enabled: &[bool; NUM_STEMS]) -> StemGains {
    Arc::new(std::array::from_fn(|i| {
        std::sync::atomic::AtomicU32::new(if enabled[i] { 1.0f32 } else { 0.0f32 }.to_bits())
    }))
}

/// Set stem `index` fully on or off. Muting is a gain of `0.0`; the
/// mixer's ramp turns the step into a short fade.
pub fn set_stem_gain(gains: &StemGains, index: usize, on: bool) {
    gains[index].store(
        if on { 1.0f32 } else { 0.0f32 }.to_bits(),
        std::sync::atomic::Ordering::Relaxed,
    );
}

/// Read the current target gain for stem `index`.
pub fn stem_gain(gains: &StemGains, index: usize) -> f32 {
    f32::from_bits(gains[index].load(std::sync::atomic::Ordering::Relaxed))
}

#[derive(Debug)]
pub enum StemError {
    /// The engine binary could not be spawned (missing, not executable).
    Spawn(String),
    /// The engine exited non-zero. `stderr` carries its diagnostic tail;
    /// `exit_code` is `None` when terminated by signal.
    EngineFailed {
        exit_code: Option<i32>,
        stderr: String,
    },
    /// User cancelled mid-separation. Partial output is the caller's to
    /// discard (it lives in the caller-supplied `out_dir`, never the cache).
    Cancelled,
    /// The engine exited zero but an expected stem file is missing —
    /// usually a model/filename mismatch.
    MissingOutput(PathBuf),
    Io(String),
}

impl std::fmt::Display for StemError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StemError::Spawn(e) => write!(f, "failed to spawn stem engine: {e}"),
            StemError::EngineFailed {
                exit_code: Some(c),
                stderr,
            } => write!(f, "stem engine exited with status {c}: {stderr}"),
            StemError::EngineFailed {
                exit_code: None,
                stderr,
            } => write!(f, "stem engine terminated without status: {stderr}"),
            StemError::Cancelled => write!(f, "separation cancelled by user"),
            StemError::MissingOutput(p) => {
                write!(f, "stem engine produced no {}", p.display())
            }
            StemError::Io(e) => write!(f, "stem io error: {e}"),
        }
    }
}

impl std::error::Error for StemError {}

/// A stem-separation engine. `DemucsCli` is the only implementation today;
/// the trait keeps the background worker and cache agnostic so an
/// in-process backend can replace the subprocess later.
pub trait StemSeparator {
    /// Cheap availability probe, called at `M`-press time (never startup).
    /// The `Err` string is shown to the user verbatim.
    fn available(&self) -> Result<(), String>;

    /// Separate `src` into `out_dir`, blocking until done.
    ///
    /// `cancelled` is polled every ~100 ms and kills the engine when it
    /// returns true (the `rip_track_cancellable` contract). `progress`
    /// receives 0..=100 ticks parsed from the engine's output; callers
    /// must tolerate sparse or absent ticks. `on_line` receives the
    /// engine's informational output (model download notices, per-track
    /// banners, warnings — progress bars excluded) so a stalled or
    /// misbehaving engine is diagnosable from the app's log instead of
    /// silently eating CPU.
    fn separate(
        &self,
        src: &Path,
        out_dir: &Path,
        cancelled: &dyn Fn() -> bool,
        progress: &dyn Fn(u8),
        on_line: &dyn Fn(&str),
    ) -> Result<StemSet, StemError>;
}

/// Shell-out to the `demucs` CLI (or a compatible fork — the binary and
/// model both come from `[stems]` config).
pub struct DemucsCli {
    pub command: PathBuf,
    pub model: String,
}

/// Build the demucs argv tail: `-n <model> --flac -o <out_dir> <input>`.
///
/// `--flac` selects lossless output at roughly half the disk of demucs'
/// default WAV; the muxer choice must agree with [`STEM_EXT`] because
/// [`expected_output_dir`] assembles the paths from it.
pub fn build_demucs_args(model: &str, out_dir: &Path, input: &Path) -> Vec<String> {
    vec![
        "-n".into(),
        model.to_string(),
        "--flac".into(),
        "-o".into(),
        out_dir.to_string_lossy().into_owned(),
        input.to_string_lossy().into_owned(),
    ]
}

/// Where demucs writes the stems for `input`: `<out_dir>/<model>/<track>/`
/// where `<track>` is the input's file stem.
pub fn expected_output_dir(out_dir: &Path, model: &str, input: &Path) -> PathBuf {
    let track = input.file_stem().unwrap_or_default().to_string_lossy();
    out_dir.join(model).join(track.as_ref())
}

/// Extract the most recent percentage tick from a chunk of demucs stderr.
///
/// demucs renders tqdm progress bars, `\r`-rewritten lines shaped like
/// ` 26%|██▌       | 56.7/218.7 [00:12<00:35, 4.60seconds/s]`, so this
/// scans raw chunk text (not lines) and returns the last `NN%` whose
/// digits parse and clamp to 0..=100. Returns `None` when the chunk has
/// no tick — callers keep the previous value.
pub fn parse_progress_percent(chunk: &str) -> Option<u8> {
    let bytes = chunk.as_bytes();
    let mut last: Option<u8> = None;
    for (i, b) in bytes.iter().enumerate() {
        if *b != b'%' {
            continue;
        }
        // Walk back over up to three ASCII digits.
        let mut start = i;
        while start > 0 && i - start < 3 && bytes[start - 1].is_ascii_digit() {
            start -= 1;
        }
        if start == i {
            continue; // '%' with no digits before it
        }
        if start > 0 && bytes[start - 1].is_ascii_digit() {
            // A digit run longer than three ("1000%") isn't a percentage;
            // taking its last three digits would report a bogus tick.
            continue;
        }
        if let Ok(pct) = chunk[start..i].parse::<u16>() {
            if pct <= 100 {
                last = Some(pct as u8);
            }
        }
    }
    last
}

/// [`parse_progress_percent`] over a raw pipe stream: prepends the bytes
/// carried from the previous read so a `NN%` straddling a 4096-byte
/// chunk boundary isn't parsed as its trailing digits alone (` 26%`
/// split after the `2` used to report a bogus 6% and visibly rewind the
/// progress display). Keeps a few tail bytes for the next call.
fn parse_progress_across_chunks(carry: &mut Vec<u8>, chunk: &[u8]) -> Option<u8> {
    let mut buf = std::mem::take(carry);
    buf.extend_from_slice(chunk);
    let pct = parse_progress_percent(&String::from_utf8_lossy(&buf));
    // 8 bytes comfortably covers a boundary-split " 100" plus a partial
    // UTF-8 code point; re-parsing an already-reported tick from the
    // carry is harmless (same value wins again).
    let keep_from = buf.len().saturating_sub(8);
    buf.drain(..keep_from);
    *carry = buf;
    pct
}

/// Isolate an engine/installer child for clean teardown. Two layers:
///
/// - **Own process group** (unix): the child (and everything it spawns —
///   `curl | sh` pipeline stages, demucs' own workers) lands in one pgid,
///   so cancellation can [`kill_child_group`] the whole tree. Killing
///   only the direct child leaves grandchildren orphaned: a cancelled
///   `sh -c "curl … | sh"` bootstrap kept downloading and installing.
/// - **`PR_SET_PDEATHSIG`** (Linux): the direct child dies with this
///   process even on hard death (SIGKILL, panic-abort). An orphaned
///   demucs keeps holding the HuggingFace model-download file lock —
///   silently wedging every future separation on the machine until
///   someone finds and kills it.
///
/// macOS has no PDEATHSIG; the normal quit path covers it instead —
/// [`kill_active_stem_children`] runs when the TUI run loop exits and
/// SIGKILLs every registered child group. Only a hard kill of the TUI
/// (SIGKILL, power loss) can still orphan the engine on macOS.
pub(crate) fn isolate_child_process(cmd: &mut std::process::Command) {
    // A child in its own process group must never read the TUI's
    // terminal: the kernel answers a background-group TTY read with
    // SIGTTIN, which STOPS the whole group. demucs hit exactly this —
    // its m4a decode shells out to ffmpeg, which reads stdin for
    // interactive commands by default, freezing every separation of a
    // non-wav source at "Separating track…". Null stdin so any such
    // read sees instant EOF instead.
    cmd.stdin(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setpgid(0,0) and prctl are async-signal-safe and the
        // closure does nothing else — the canonical pre_exec use. The
        // new group also detaches the engine from terminal job-control
        // signals, which is fine: the TUI runs raw-mode and delivers
        // cancellation explicitly.
        unsafe {
            cmd.pre_exec(|| {
                libc::setpgid(0, 0);
                #[cfg(target_os = "linux")]
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                Ok(())
            });
        }
    }
    #[cfg(not(unix))]
    {
        let _ = cmd;
    }
}

/// SIGKILL `child`'s entire process group (unix), falling back to the
/// direct child, then reap it. Pairs with [`isolate_child_process`] —
/// without the group kill, pipeline grandchildren survive a cancel and
/// the pipe drain threads never see EOF (the orphans inherit the write
/// ends), so the cancelling thread blocks until the orphan finishes.
pub(crate) fn kill_child_group(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        let pid = child.id() as libc::pid_t;
        // SAFETY: plain kill(2) on the group we created at spawn. The
        // child is unreaped (we hold the handle), so the pid can't have
        // been reused.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Pids of engine/installer children currently running, so the TUI quit
/// path can take the whole set down. Registered via [`ChildGroupGuard`]
/// by the spawn sites in this module and `provision.rs` — a process-wide
/// registry because the children are spawned deep inside the lib crate
/// while the quit decision happens in the TUI binary.
static ACTIVE_CHILD_PIDS: std::sync::OnceLock<Mutex<Vec<u32>>> = std::sync::OnceLock::new();

fn active_child_pids() -> &'static Mutex<Vec<u32>> {
    ACTIVE_CHILD_PIDS.get_or_init(|| Mutex::new(Vec::new()))
}

/// RAII registration of a spawned engine/installer child in the
/// process-wide registry. Held by the job thread for the child's
/// lifetime; dropping (normal reap path) deregisters.
pub(crate) struct ChildGroupGuard {
    pid: u32,
}

impl ChildGroupGuard {
    pub(crate) fn register(child: &std::process::Child) -> Self {
        let pid = child.id();
        if let Ok(mut pids) = active_child_pids().lock() {
            pids.push(pid);
        }
        ChildGroupGuard { pid }
    }
}

impl Drop for ChildGroupGuard {
    fn drop(&mut self) {
        if let Ok(mut pids) = active_child_pids().lock() {
            pids.retain(|p| *p != self.pid);
        }
    }
}

/// SIGKILL every registered engine/installer child group. Called by the
/// TUI when the run loop exits so quitting mid-separation (or
/// mid-install) never orphans a demucs/uv tree — the macOS answer to
/// Linux's PDEATHSIG, and the fix for the orphan that wedged the
/// HuggingFace download lock. Idempotent; racing the job thread's own
/// kill is harmless (the pid stays valid until the holder reaps it).
pub fn kill_active_stem_children() {
    let pids: Vec<u32> = match active_child_pids().lock() {
        Ok(p) => p.clone(),
        Err(_) => return,
    };
    for pid in pids {
        #[cfg(unix)]
        // SAFETY: see kill_child_group — unreaped children, so no reuse.
        unsafe {
            libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
            libc::kill(pid as libc::pid_t, libc::SIGKILL);
        }
        #[cfg(not(unix))]
        let _ = pid;
    }
}

/// Pull complete informational lines out of a pipe stream. `pending`
/// carries a partial line across chunk boundaries; `\r` counts as a line
/// terminator so tqdm rewrites don't glue everything together. Progress
/// bars (`%|`) and blank lines are dropped — they go through the percent
/// channel, not the log.
fn extract_info_lines(pending: &mut Vec<u8>, chunk: &[u8]) -> Vec<String> {
    pending.extend_from_slice(chunk);
    let mut lines = Vec::new();
    while let Some(pos) = pending.iter().position(|b| *b == b'\n' || *b == b'\r') {
        let raw: Vec<u8> = pending.drain(..=pos).collect();
        let text = String::from_utf8_lossy(&raw[..raw.len() - 1])
            .trim()
            .to_string();
        if !text.is_empty() && !text.contains("%|") {
            lines.push(text);
        }
    }
    lines
}

/// Trim a captured stderr buffer down to the interesting tail for error
/// reporting: tqdm progress lines (anything carrying a `%|` bar) are
/// dropped, `\r` rewrites are treated as line breaks, and only the last
/// few remaining lines are kept.
fn stderr_tail(raw: &[u8]) -> String {
    let text = String::from_utf8_lossy(raw);
    let lines: Vec<&str> = text
        .split(['\n', '\r'])
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.contains("%|"))
        .collect();
    let start = lines.len().saturating_sub(8);
    lines[start..].join("\n")
}

impl StemSeparator for DemucsCli {
    fn available(&self) -> Result<(), String> {
        // `--help` is argparse-only (no torch import) so this stays fast.
        match std::process::Command::new(&self.command)
            .arg("--help")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
        {
            Ok(s) if s.success() => Ok(()),
            Ok(s) => Err(format!(
                "{} --help exited with {:?}",
                self.command.display(),
                s.code()
            )),
            Err(e) => Err(format!(
                "stem engine not found at {}: {e}",
                self.command.display()
            )),
        }
    }

    fn separate(
        &self,
        src: &Path,
        out_dir: &Path,
        cancelled: &dyn Fn() -> bool,
        progress: &dyn Fn(u8),
        on_line: &dyn Fn(&str),
    ) -> Result<StemSet, StemError> {
        std::fs::create_dir_all(out_dir).map_err(|e| StemError::Io(e.to_string()))?;
        let args = build_demucs_args(&self.model, out_dir, src);
        let mut cmd = std::process::Command::new(&self.command);
        cmd.args(&args)
            // Python block-buffers stdout when it's a pipe, so demucs'
            // informational banners ("Downloading:", "Separating track…")
            // would sit in its own buffer until exit and never reach the
            // log mid-run. Force unbuffered output.
            .env("PYTHONUNBUFFERED", "1")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        isolate_child_process(&mut cmd);
        let mut child = cmd.spawn().map_err(|e| StemError::Spawn(e.to_string()))?;
        let _group = ChildGroupGuard::register(&child);

        // demucs writes its tqdm progress to stderr and informational
        // lines ("Selected model…", "Separating track…", download
        // notices) to stdout. Both pipes are drained continuously so
        // neither can fill and deadlock the child (the CD-rip lesson);
        // stderr chunks additionally feed percentage ticks, and both
        // pipes feed informational lines, through channels the
        // cancel-poll loop below drains — no extra timer thread.
        let (pct_tx, pct_rx) = std::sync::mpsc::channel::<u8>();
        let (line_tx, line_rx) = std::sync::mpsc::channel::<String>();
        // ZYTUNES_STEM_DEBUG=1 tees the engine's raw, unfiltered output
        // (both pipes, interleaved) to a file — ground truth for "is the
        // engine silent, or are we losing its output on the way in".
        let debug_tee: Option<Arc<Mutex<std::fs::File>>> = std::env::var_os("ZYTUNES_STEM_DEBUG")
            .and_then(|_| {
                std::fs::File::create(std::env::temp_dir().join("zytunes-stem-engine.log"))
                    .ok()
                    .map(|f| Arc::new(Mutex::new(f)))
            });
        let tee = |dst: &Option<Arc<Mutex<std::fs::File>>>, bytes: &[u8]| {
            if let Some(f) = dst {
                if let Ok(mut f) = f.lock() {
                    use std::io::Write;
                    let _ = f.write_all(bytes);
                    let _ = f.flush();
                }
            }
        };
        let stderr_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        let stderr_handle = child.stderr.take().map(|mut pipe| {
            let buf = Arc::clone(&stderr_buf);
            let lines_tx = line_tx.clone();
            let dbg = debug_tee.clone();
            std::thread::spawn(move || {
                use std::io::Read;
                let mut chunk = [0u8; 4096];
                let mut pending = Vec::new();
                let mut pct_carry = Vec::new();
                loop {
                    match pipe.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            tee(&dbg, &chunk[..n]);
                            if let Some(pct) =
                                parse_progress_across_chunks(&mut pct_carry, &chunk[..n])
                            {
                                let _ = pct_tx.send(pct);
                            }
                            for line in extract_info_lines(&mut pending, &chunk[..n]) {
                                let _ = lines_tx.send(line);
                            }
                            if let Ok(mut b) = buf.lock() {
                                b.extend_from_slice(&chunk[..n]);
                                // Only the filtered tail is ever read (the
                                // last few lines on the failure path), but
                                // tqdm rewrites its bar many times a second
                                // for the whole multi-minute run — keep a
                                // bounded window, not the full stream.
                                if b.len() > 64 * 1024 {
                                    let cut = b.len() - 32 * 1024;
                                    b.drain(..cut);
                                }
                            }
                        }
                    }
                }
            })
        });
        let stdout_handle = child.stdout.take().map(|mut pipe| {
            let lines_tx = line_tx;
            let dbg = debug_tee.clone();
            std::thread::spawn(move || {
                use std::io::Read;
                let mut chunk = [0u8; 4096];
                let mut pending = Vec::new();
                loop {
                    match pipe.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            tee(&dbg, &chunk[..n]);
                            for line in extract_info_lines(&mut pending, &chunk[..n]) {
                                let _ = lines_tx.send(line);
                            }
                        }
                    }
                }
            })
        });

        let drain_pct = |rx: &std::sync::mpsc::Receiver<u8>| {
            while let Ok(pct) = rx.try_recv() {
                progress(pct);
            }
        };
        let drain_lines = |rx: &std::sync::mpsc::Receiver<String>| {
            while let Ok(line) = rx.try_recv() {
                on_line(&line);
            }
        };
        let exit_outcome: Result<(), StemError> = loop {
            drain_pct(&pct_rx);
            drain_lines(&line_rx);
            if cancelled() {
                // Cancel-during-completion race: accept a child that
                // already finished successfully instead of discarding a
                // completed separation.
                if let Ok(Some(status)) = child.try_wait() {
                    if status.success() {
                        break Ok(());
                    }
                    break Err(StemError::EngineFailed {
                        exit_code: status.code(),
                        stderr: String::new(),
                    });
                }
                kill_child_group(&mut child);
                break Err(StemError::Cancelled);
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    if status.success() {
                        break Ok(());
                    }
                    break Err(StemError::EngineFailed {
                        exit_code: status.code(),
                        stderr: String::new(), // populated below from the drained buffer
                    });
                }
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(100)),
                Err(e) => break Err(StemError::Spawn(e.to_string())),
            }
        };

        if let Some(h) = stderr_handle {
            let _ = h.join();
        }
        if let Some(h) = stdout_handle {
            let _ = h.join();
        }
        drain_pct(&pct_rx);
        drain_lines(&line_rx);

        match exit_outcome {
            Ok(()) => {
                let dir = expected_output_dir(out_dir, &self.model, src);
                let set = StemSet::from_dir(&dir, STEM_EXT);
                for p in &set.paths {
                    if !p.is_file() {
                        return Err(StemError::MissingOutput(p.clone()));
                    }
                }
                progress(100);
                Ok(set)
            }
            Err(StemError::EngineFailed { exit_code, .. }) => {
                let stderr = stderr_buf
                    .lock()
                    .map(|b| stderr_tail(&b))
                    .unwrap_or_default();
                Err(StemError::EngineFailed { exit_code, stderr })
            }
            Err(other) => Err(other),
        }
    }
}

// ---------------------------------------------------------------------------
// Stem cache
// ---------------------------------------------------------------------------

/// Sidecar written last into each cache entry; its presence marks the
/// entry valid, its `(mtime, size, model)` triple decides staleness, and
/// its own file mtime is the LRU clock for [`prune_stem_cache`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StemCacheMeta {
    /// Absolute path of the source track (diagnostic only — the entry is
    /// keyed by its hash, so collisions resolve via fingerprint mismatch).
    pub source_path: String,
    pub fingerprint: FileFingerprint,
    pub model: String,
}

const META_NAME: &str = "meta.json";

/// Default stem cache root. Lives under `$HOME/.cache/zytunes` beside the
/// dirlib and art caches, and — like them — deliberately ignores
/// `ZYTUNES_CACHE_DIR`: stems are derived purely from source audio, so
/// sibling worktrees pointed at the same `~/Music` should share one copy
/// of these large files.
pub fn default_stem_cache_dir() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(
        Path::new(&home)
            .join(".cache")
            .join("zytunes")
            .join("stems"),
    )
}

/// Cache entry directory name for a source path — same `DefaultHasher`
/// scheme the dirlib cache uses for scan roots.
pub fn stem_cache_key(source_path: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    source_path.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn read_meta(entry_dir: &Path) -> Option<StemCacheMeta> {
    let data = std::fs::read(entry_dir.join(META_NAME)).ok()?;
    serde_json::from_slice(&data).ok()
}

fn write_meta(entry_dir: &Path, meta: &StemCacheMeta) -> Result<(), String> {
    let data = serde_json::to_vec(meta).map_err(|e| format!("serialize stem meta: {e}"))?;
    crate::paths::atomic_write_json(&entry_dir.join(META_NAME), &data)
}

/// Look up cached stems for `source` under `cache_dir`.
///
/// A hit requires the meta sidecar to parse, its fingerprint to match the
/// source file's current `(mtime, size)`, its model to match `model`, and
/// all six stem files to exist. On a hit the meta is rewritten in place to
/// bump its mtime — the LRU touch. Any mismatch is a miss (the caller
/// re-separates and [`store_stems`] replaces the entry).
pub fn cached_stems(cache_dir: &Path, source: &Path, model: &str, log: &Logger) -> Option<StemSet> {
    let entry_dir = cache_dir.join(stem_cache_key(&source.to_string_lossy()));
    let meta = read_meta(&entry_dir)?;
    let current = FileFingerprint::from_path(source)?;
    if meta.fingerprint != current || meta.model != model {
        return None;
    }
    let set = StemSet::from_dir(&entry_dir, STEM_EXT);
    if !set.all_exist() {
        log(&format!(
            "stem cache entry {} is incomplete; will re-separate",
            entry_dir.display()
        ));
        return None;
    }
    if let Err(e) = write_meta(&entry_dir, &meta) {
        // Failing to bump the LRU clock is harmless; the hit still counts.
        log(&format!("stem cache touch failed: {e}"));
    }
    Some(set)
}

/// Move freshly separated stems into the cache and prune to `max_bytes`.
///
/// The entry is staged in `{key}.tmp` with the meta sidecar written last,
/// then swapped into place — a crash mid-store leaves either the previous
/// entry or a metaless (hence invisible) stage, never a half-entry that
/// [`cached_stems`] would serve. The returned [`StemSet`] points at the
/// cached copies; `produced`'s files have been moved away.
///
/// The freshly stored entry is exempt from this prune pass so a cache cap
/// smaller than one track still plays (it gets evicted by the next store).
pub fn store_stems(
    cache_dir: &Path,
    source: &Path,
    model: &str,
    produced: &StemSet,
    max_bytes: u64,
    log: &Logger,
) -> Result<StemSet, String> {
    let fingerprint = FileFingerprint::from_path(source)
        .ok_or_else(|| format!("cannot stat source {}", source.display()))?;
    let key = stem_cache_key(&source.to_string_lossy());
    let entry_dir = cache_dir.join(&key);
    let stage_dir = cache_dir.join(format!("{key}.tmp"));

    let _ = std::fs::remove_dir_all(&stage_dir);
    std::fs::create_dir_all(&stage_dir)
        .map_err(|e| format!("mkdir {} failed: {e}", stage_dir.display()))?;
    let staged = StemSet::from_dir(&stage_dir, STEM_EXT);
    for (src_path, dst_path) in produced.paths.iter().zip(staged.paths.iter()) {
        // rename() first (same filesystem when the worker stages under the
        // cache root); fall back to copy+remove across mount points.
        if std::fs::rename(src_path, dst_path).is_err() {
            std::fs::copy(src_path, dst_path).map_err(|e| {
                format!(
                    "move {} -> {} failed: {e}",
                    src_path.display(),
                    dst_path.display()
                )
            })?;
            let _ = std::fs::remove_file(src_path);
        }
    }
    write_meta(
        &stage_dir,
        &StemCacheMeta {
            source_path: source.to_string_lossy().into_owned(),
            fingerprint,
            model: model.to_string(),
        },
    )?;

    let _ = std::fs::remove_dir_all(&entry_dir);
    std::fs::rename(&stage_dir, &entry_dir).map_err(|e| {
        format!(
            "rename {} -> {} failed: {e}",
            stage_dir.display(),
            entry_dir.display()
        )
    })?;

    prune_stem_cache(cache_dir, max_bytes, Some(&key), log);
    Ok(StemSet::from_dir(&entry_dir, STEM_EXT))
}

fn dir_size_bytes(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum()
}

/// Evict least-recently-used cache entries until the total size fits in
/// `max_bytes`. "Recently used" is the meta sidecar's mtime, bumped on
/// every hit by [`cached_stems`]. `keep_key` (the just-stored entry) is
/// never evicted. Stray `.tmp` stages from crashed stores are always
/// removed. Every eviction is logged — no silent caps.
pub fn prune_stem_cache(cache_dir: &Path, max_bytes: u64, keep_key: Option<&str>, log: &Logger) {
    let Ok(entries) = std::fs::read_dir(cache_dir) else {
        return;
    };

    // (meta mtime, dir, size) for valid entries; stage dirs get cleaned.
    let mut valid: Vec<(std::time::SystemTime, PathBuf, u64)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if path.extension().is_some_and(|e| e == "tmp") {
            let _ = std::fs::remove_dir_all(&path);
            continue;
        }
        let meta_path = path.join(META_NAME);
        let Ok(meta_mtime) = std::fs::metadata(&meta_path).and_then(|m| m.modified()) else {
            // No readable meta sidecar → a crashed store or foreign dir;
            // leave it alone rather than deleting something we don't own.
            continue;
        };
        let size = dir_size_bytes(&path);
        valid.push((meta_mtime, path, size));
    }

    let mut total: u64 = valid.iter().map(|(_, _, s)| s).sum();
    if total <= max_bytes {
        return;
    }
    // Oldest first.
    valid.sort_by_key(|(mtime, _, _)| *mtime);
    for (_, path, size) in valid {
        if total <= max_bytes {
            break;
        }
        if let Some(keep) = keep_key {
            if path.file_name().is_some_and(|n| n == keep) {
                continue;
            }
        }
        match std::fs::remove_dir_all(&path) {
            Ok(()) => {
                total = total.saturating_sub(size);
                log(&format!(
                    "stem cache: evicted {} ({:.1} MB) to fit the cache cap",
                    path.display(),
                    size as f64 / (1024.0 * 1024.0)
                ));
            }
            Err(e) => log(&format!(
                "stem cache: failed to evict {}: {e}",
                path.display()
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::default_logger;

    fn temp_root(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "zytunes-stems-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    /// Poll `pred` every 20 ms for up to ~5 s; panic with `what` if it
    /// never turns true. Keeps the subprocess tests bounded instead of
    /// hanging CI on a regression.
    #[cfg(unix)]
    fn wait_until(what: &str, pred: impl Fn() -> bool) {
        for _ in 0..250 {
            if pred() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!("timed out waiting for {what}");
    }

    /// `kill(pid, 0)` existence probe: false once the process is gone.
    #[cfg(unix)]
    fn pid_alive(pid: libc::pid_t) -> bool {
        // SAFETY: signal 0 delivers nothing; pure existence check.
        unsafe { libc::kill(pid, 0) == 0 }
    }

    #[test]
    #[cfg(unix)]
    fn kill_child_group_takes_down_pipeline_grandchildren() {
        // The uv bootstrap is `sh -c "curl … | sh"`: killing only the
        // direct sh leaves the pipeline stages running (the original
        // cancel bug — the install completed against the user's cancel).
        // Model the shape with a shell that spawns a long-lived
        // grandchild and reports its pid.
        let root = temp_root("pgkill");
        let pidfile = root.join("grandchild.pid");
        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-c").arg(format!(
            "sleep 30 & echo $! > {}; wait",
            pidfile.to_string_lossy()
        ));
        isolate_child_process(&mut cmd);
        let mut child = cmd.spawn().expect("spawn sh");

        wait_until("grandchild pid file", || {
            std::fs::read_to_string(&pidfile).is_ok_and(|s| !s.trim().is_empty())
        });
        let grandchild: libc::pid_t = std::fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .parse()
            .expect("pid parses");
        assert!(pid_alive(grandchild), "grandchild running before the kill");

        kill_child_group(&mut child);
        wait_until("grandchild death", || !pid_alive(grandchild));
    }

    #[test]
    #[cfg(unix)]
    fn isolated_child_stdin_is_null_not_the_terminal() {
        // `read` off a null stdin sees instant EOF; off an inherited
        // terminal it would block (and in a background process group the
        // kernel would SIGTTIN-stop the child — the frozen-demucs bug).
        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-c").arg("read -r line; exit 42");
        isolate_child_process(&mut cmd);
        let child = std::sync::Mutex::new(cmd.spawn().expect("spawn sh"));
        wait_until("child exits on stdin EOF", || {
            child.lock().unwrap().try_wait().is_ok_and(|s| s.is_some())
        });
        let status = child.into_inner().unwrap().wait().expect("wait");
        assert_eq!(
            status.code(),
            Some(42),
            "read must fail with EOF, not block"
        );
    }

    #[test]
    #[cfg(unix)]
    fn quit_teardown_kills_registered_children_and_guard_deregisters() {
        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-c").arg("sleep 30");
        isolate_child_process(&mut cmd);
        let mut child = cmd.spawn().expect("spawn sh");
        let pid = child.id();
        {
            let _guard = ChildGroupGuard::register(&child);
            assert!(active_child_pids().lock().unwrap().contains(&pid));

            // The TUI quit path: every registered child group dies.
            kill_active_stem_children();
            let status = child.wait().expect("reap");
            assert!(!status.success(), "sleeper was killed, not finished");
        }
        assert!(
            !active_child_pids().lock().unwrap().contains(&pid),
            "guard drop must deregister"
        );
    }

    /// Write a fake source track and a produced-stems dir, returning both.
    fn fake_separation(root: &Path, body: &[u8]) -> (PathBuf, StemSet) {
        let source = root.join("song.mp3");
        std::fs::write(&source, body).unwrap();
        let produced_dir = root.join("produced");
        std::fs::create_dir_all(&produced_dir).unwrap();
        let produced = StemSet::from_dir(&produced_dir, STEM_EXT);
        for p in &produced.paths {
            std::fs::write(p, b"flacdata").unwrap();
        }
        (source, produced)
    }

    #[test]
    fn stem_kind_order_matches_indices_and_filenames() {
        // UI order (keys 1-6), gains indices, and demucs filenames all
        // key off this array — a re-order must fail loudly.
        let expected = ["vocals", "drums", "bass", "guitar", "piano", "other"];
        assert_eq!(StemKind::ALL.len(), NUM_STEMS);
        for (i, kind) in StemKind::ALL.iter().enumerate() {
            assert_eq!(kind.index(), i);
            assert_eq!(kind.file_stem(), expected[i]);
            assert_eq!(kind.short_label().len(), 3, "strip layout assumes 3 chars");
        }
    }

    #[test]
    fn stem_gains_roundtrip_and_toggle() {
        let gains = new_stem_gains(&[true, false, true, true, true, true]);
        assert_eq!(stem_gain(&gains, 0), 1.0);
        assert_eq!(stem_gain(&gains, 1), 0.0);
        set_stem_gain(&gains, 0, false);
        set_stem_gain(&gains, 1, true);
        assert_eq!(stem_gain(&gains, 0), 0.0);
        assert_eq!(stem_gain(&gains, 1), 1.0);
        // A clone shares the same atomics — the mixer's view updates.
        let mixer_view = gains.clone();
        set_stem_gain(&gains, 5, false);
        assert_eq!(stem_gain(&mixer_view, 5), 0.0);
    }

    #[test]
    fn stem_set_from_dir_assembles_demucs_layout() {
        let set = StemSet::from_dir(Path::new("/cache/abc"), "flac");
        assert_eq!(set.paths[0], Path::new("/cache/abc/vocals.flac"));
        assert_eq!(set.paths[5], Path::new("/cache/abc/other.flac"));
    }

    #[test]
    fn build_args_shape() {
        let args = build_demucs_args(
            "htdemucs_6s",
            Path::new("/tmp/out"),
            Path::new("/music/a.flac"),
        );
        assert_eq!(
            args,
            vec![
                "-n",
                "htdemucs_6s",
                "--flac",
                "-o",
                "/tmp/out",
                "/music/a.flac"
            ]
        );
    }

    #[test]
    fn expected_output_dir_uses_model_and_track_stem() {
        let dir = expected_output_dir(
            Path::new("/tmp/out"),
            "htdemucs_6s",
            Path::new("/music/Artist/01 - Song.flac"),
        );
        assert_eq!(dir, Path::new("/tmp/out/htdemucs_6s/01 - Song"));
    }

    #[test]
    fn parse_progress_handles_tqdm_chunks() {
        // Typical tqdm rewrite chunk: several \r-separated frames; the
        // last percentage wins.
        let chunk = " 12%|█▏        | 26.1/218.7\r 26%|██▌       | 56.7/218.7 [00:12<00:35,  4.60seconds/s]";
        assert_eq!(parse_progress_percent(chunk), Some(26));
        assert_eq!(parse_progress_percent("100%|██████████|"), Some(100));
        assert_eq!(parse_progress_percent("  5%|▌"), Some(5));
        // No tick in the chunk → None (caller keeps previous value).
        assert_eq!(parse_progress_percent("Separating track song.mp3"), None);
        // A bare '%' without digits is not a tick.
        assert_eq!(parse_progress_percent("weird % sign"), None);
        // >100 can only come from garbage; ignore it.
        assert_eq!(parse_progress_percent("999%|"), None);
        // ...but a valid tick elsewhere in the same chunk still counts.
        assert_eq!(parse_progress_percent("999%| then 42%|"), Some(42));
        // A digit run longer than three isn't a percentage — taking its
        // last three digits used to report "1000%" as 0 and "2050%" as
        // 50, visibly rewinding the strip.
        assert_eq!(parse_progress_percent("1000%|"), None);
        assert_eq!(parse_progress_percent("2050%|"), None);
    }

    #[test]
    fn parse_progress_survives_chunk_boundary_splits() {
        // ' 26%' split mid-number across two pipe reads: chunk 2 alone
        // would parse its trailing '6%' as 6 and rewind the display. The
        // carry re-joins the digits.
        let mut carry = Vec::new();
        assert_eq!(
            parse_progress_across_chunks(&mut carry, b" 12%|frame\r 2"),
            Some(12)
        );
        assert_eq!(
            parse_progress_across_chunks(&mut carry, b"6%|frame"),
            Some(26)
        );

        // Split exactly before the '%' too.
        let mut carry = Vec::new();
        assert_eq!(parse_progress_across_chunks(&mut carry, b" 84"), None);
        assert_eq!(parse_progress_across_chunks(&mut carry, b"%|"), Some(84));

        // Digit-run rejection still applies across the boundary: "1000%"
        // split as "10" + "00%" must not become 0.
        let mut carry = Vec::new();
        assert_eq!(parse_progress_across_chunks(&mut carry, b" 10"), None);
        assert_eq!(parse_progress_across_chunks(&mut carry, b"00%|"), None);
    }

    #[test]
    fn extract_info_lines_splits_and_filters() {
        let mut pending = Vec::new();
        // Partial line held across chunks; progress bars and blanks dropped.
        let first = extract_info_lines(&mut pending, b"Downloading: \"https://x/htde");
        assert!(first.is_empty(), "incomplete line must wait for its end");
        let second = extract_info_lines(
            &mut pending,
            b"mucs_6s.th\"\n 10%|#  | bar\r\nSeparating track song.mp3\n",
        );
        assert_eq!(
            second,
            vec![
                "Downloading: \"https://x/htdemucs_6s.th\"".to_string(),
                "Separating track song.mp3".to_string(),
            ]
        );
    }

    #[test]
    fn stderr_tail_drops_progress_bars_and_keeps_diagnostics() {
        let raw = b" 10%|#         | x\r 20%|##        | y\nTraceback (most recent call last):\n  ValueError: bad model\n";
        let tail = stderr_tail(raw);
        assert!(tail.contains("ValueError: bad model"));
        assert!(!tail.contains("%|"));
    }

    #[test]
    fn cache_key_is_stable_and_path_sensitive() {
        let a = stem_cache_key("/music/a.mp3");
        assert_eq!(a, stem_cache_key("/music/a.mp3"));
        assert_ne!(a, stem_cache_key("/music/b.mp3"));
        assert_eq!(a.len(), 16, "fixed-width hex dirname");
    }

    #[test]
    fn store_then_hit_roundtrip() {
        let root = temp_root("roundtrip");
        let cache = root.join("cache");
        let (source, produced) = fake_separation(&root, b"mp3data");
        let log = default_logger();

        let stored = store_stems(&cache, &source, "htdemucs_6s", &produced, u64::MAX, &log)
            .expect("store succeeds");
        assert!(stored.all_exist());
        // The produced files were moved, not copied.
        assert!(!produced.paths[0].exists());

        let hit = cached_stems(&cache, &source, "htdemucs_6s", &log).expect("cache hit");
        assert_eq!(hit, stored);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn source_change_and_model_change_invalidate() {
        let root = temp_root("invalidate");
        let cache = root.join("cache");
        let (source, produced) = fake_separation(&root, b"mp3data");
        let log = default_logger();
        store_stems(&cache, &source, "htdemucs_6s", &produced, u64::MAX, &log).unwrap();

        // Different model → miss even though files are present.
        assert!(cached_stems(&cache, &source, "htdemucs", &log).is_none());

        // Re-written source with a different size → fingerprint miss.
        std::fs::write(&source, b"mp3data-but-longer").unwrap();
        assert!(cached_stems(&cache, &source, "htdemucs_6s", &log).is_none());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_stem_file_is_a_miss() {
        let root = temp_root("missing");
        let cache = root.join("cache");
        let (source, produced) = fake_separation(&root, b"mp3data");
        let log = default_logger();
        let stored =
            store_stems(&cache, &source, "htdemucs_6s", &produced, u64::MAX, &log).unwrap();

        std::fs::remove_file(&stored.paths[3]).unwrap();
        assert!(cached_stems(&cache, &source, "htdemucs_6s", &log).is_none());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn prune_evicts_oldest_first_and_protects_keep_key() {
        let root = temp_root("prune");
        let cache = root.join("cache");
        std::fs::create_dir_all(&cache).unwrap();
        let log = default_logger();

        // Three entries, 8 bytes of stem data each (6 files + meta).
        // Meta mtimes are staggered by explicit rewrites with sleeps —
        // coarse (1 s) filesystem timestamps would make ordering flaky,
        // so use set-order-friendly distinct payload writes plus
        // filetime-free spacing via sleeping over the mtime granularity.
        let mut keys = Vec::new();
        for name in ["a.mp3", "b.mp3", "c.mp3"] {
            let (source, produced) = fake_separation(&temp_root(name), b"x");
            store_stems(&cache, &source, "m", &produced, u64::MAX, &log).unwrap();
            keys.push(stem_cache_key(&source.to_string_lossy()));
            std::thread::sleep(std::time::Duration::from_millis(1100));
        }

        let entry_size = dir_size_bytes(&cache.join(&keys[0]));
        assert!(entry_size > 0);

        // Cap fits exactly two entries → the oldest (a) must go.
        prune_stem_cache(&cache, entry_size * 2, None, &log);
        assert!(!cache.join(&keys[0]).exists(), "oldest evicted");
        assert!(cache.join(&keys[1]).exists());
        assert!(cache.join(&keys[2]).exists());

        // Cap fits one entry but the oldest survivor is protected by
        // keep_key → the other one goes instead.
        prune_stem_cache(&cache, entry_size, Some(&keys[1]), &log);
        assert!(cache.join(&keys[1]).exists(), "keep_key protected");
        assert!(!cache.join(&keys[2]).exists());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn prune_cleans_stale_stage_dirs() {
        let root = temp_root("stage");
        let cache = root.join("cache");
        let stale = cache.join("deadbeef.tmp");
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::write(stale.join("vocals.flac"), b"partial").unwrap();

        prune_stem_cache(&cache, u64::MAX, None, &default_logger());
        assert!(!stale.exists(), "crashed-store stage removed");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn store_replaces_existing_entry() {
        let root = temp_root("replace");
        let cache = root.join("cache");
        let log = default_logger();
        let (source, produced) = fake_separation(&root, b"v1");
        store_stems(&cache, &source, "m", &produced, u64::MAX, &log).unwrap();

        // Source re-tagged → new fingerprint, fresh separation, re-store.
        std::fs::write(&source, b"v2-longer").unwrap();
        let produced_dir = root.join("produced2");
        std::fs::create_dir_all(&produced_dir).unwrap();
        let produced2 = StemSet::from_dir(&produced_dir, STEM_EXT);
        for p in &produced2.paths {
            std::fs::write(p, b"newflac").unwrap();
        }
        let stored = store_stems(&cache, &source, "m", &produced2, u64::MAX, &log).unwrap();
        assert!(stored.all_exist());
        assert_eq!(std::fs::read(&stored.paths[0]).unwrap(), b"newflac");

        let hit = cached_stems(&cache, &source, "m", &log).expect("fresh entry hits");
        assert_eq!(hit, stored);

        let _ = std::fs::remove_dir_all(&root);
    }

    /// Live-engine coverage (spawn/cancel/kill) requires a real demucs
    /// install, so it lives in the manual workflow; `available()` on a
    /// nonexistent binary is the one subprocess path testable everywhere.
    #[test]
    fn available_reports_missing_binary() {
        let sep = DemucsCli {
            command: PathBuf::from("/nonexistent/zytunes-demucs-test"),
            model: "htdemucs_6s".into(),
        };
        let err = sep.available().unwrap_err();
        assert!(err.contains("not found"), "{err}");
    }
}
