//! Engine subprocess lifecycle, shared by every stem-engine shell-out.
//!
//! [`run_engine_process`] is the single driver behind `DemucsCli`,
//! `AudioSeparatorCli`, and `provision::run_streaming` — it owns the
//! spawn hardening ([`isolate_child_process`]), the quit-teardown
//! registry ([`ChildGroupGuard`]), the continuous dual-pipe drains
//! (deadlock avoidance — the CD-rip lesson), tqdm progress parsing off
//! stderr, informational-line extraction from both pipes, the 100 ms
//! cancel poll with group kill, and the cancel-during-completion race
//! (a child that already finished keeps its real outcome). Callers map
//! [`EngineOutcome`] into their own error types.

use std::path::Path;
use std::sync::{Arc, Mutex};

/// Serialise tests that register a child in the process-wide registry
/// or sweep it. The registry is process-wide (TUI quit path), so
/// `quit_teardown_kills_registered_children` SIGKILLs in-flight
/// `run_engine_process` children from *other* tests — including the
/// cascade stubs in `stems.rs`. Hold this for any test that registers
/// a child or calls [`kill_active_stem_children`].
#[cfg(test)]
pub(crate) fn lock_driver_tests() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Hooks a caller wires into [`run_engine_process`]. All are polled or
/// invoked from the calling thread and the pipe-drain threads the driver
/// owns; none outlive the call.
pub(crate) struct EngineHooks<'a> {
    /// Polled every ~100 ms; `true` group-kills the child (unless it
    /// already exited — see [`EngineOutcome`] on the cancel race).
    pub cancelled: &'a dyn Fn() -> bool,
    /// Receives 0..=100 ticks parsed from stderr's tqdm output. `None`
    /// skips percent parsing entirely (installers have no progress bars).
    pub on_progress: Option<&'a dyn Fn(u8)>,
    /// Receives informational lines from both pipes (`\r` counts as a
    /// terminator; progress-bar lines carrying `%|` are filtered out).
    pub on_line: &'a dyn Fn(&str),
}

/// How the engine process ended, for exits the driver could observe.
/// Spawn failures are the `Err` arm of [`run_engine_process`] instead.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum EngineOutcome {
    /// Exited zero. Also chosen when a cancel raced a successful exit —
    /// a completed run is never discarded.
    Success,
    /// Exited non-zero (or was signalled: `exit_code: None`). `tail`
    /// carries the filtered last lines of stderr for diagnostics. Also
    /// chosen when a cancel raced a *failed* exit — a real failure is
    /// never masked as a user cancel.
    Failed {
        exit_code: Option<i32>,
        tail: String,
    },
    /// The cancel poll tripped while the child was still running; the
    /// whole process group was killed.
    Cancelled,
}

/// Spawn `cmd` (hardened via [`isolate_child_process`], registered for
/// quit teardown) and drive it to completion under `hooks`. `Err` is a
/// spawn failure message; every observed exit is an `Ok(EngineOutcome)`.
pub(crate) fn run_engine_process(
    cmd: &mut std::process::Command,
    hooks: &EngineHooks<'_>,
) -> Result<EngineOutcome, String> {
    // Python block-buffers stdout when it's a pipe, so an engine's
    // informational banners would sit in its own buffer until exit and
    // never reach the log mid-run. Force unbuffered output; harmless for
    // non-Python children (uv, sh).
    cmd.env("PYTHONUNBUFFERED", "1")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    isolate_child_process(cmd);
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    let _group = ChildGroupGuard::register(&child);

    // Engines write tqdm progress to stderr and informational lines to
    // either pipe. Both pipes are drained continuously so neither can
    // fill and deadlock the child; stderr chunks additionally feed
    // percentage ticks, and both pipes feed informational lines, through
    // channels the cancel-poll loop below drains — no extra timer thread.
    let (pct_tx, pct_rx) = std::sync::mpsc::channel::<u8>();
    let (line_tx, line_rx) = std::sync::mpsc::channel::<String>();
    let parse_progress = hooks.on_progress.is_some();
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
                        if parse_progress {
                            if let Some(pct) =
                                parse_progress_across_chunks(&mut pct_carry, &chunk[..n])
                            {
                                let _ = pct_tx.send(pct);
                            }
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
    // stdout is buffered too: installers report errors on either pipe
    // (the `curl | sh` uv bootstrap and uv's resolution output both use
    // stdout), and a stderr-only failure tail was surfacing those runs
    // as a bare exit code.
    let stdout_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let stdout_handle = child.stdout.take().map(|mut pipe| {
        let buf = Arc::clone(&stdout_buf);
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
                        if let Ok(mut b) = buf.lock() {
                            b.extend_from_slice(&chunk[..n]);
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

    let drain_pct = |rx: &std::sync::mpsc::Receiver<u8>| {
        if let Some(progress) = hooks.on_progress {
            while let Ok(pct) = rx.try_recv() {
                progress(pct);
            }
        }
    };
    let drain_lines = |rx: &std::sync::mpsc::Receiver<String>| {
        while let Ok(line) = rx.try_recv() {
            (hooks.on_line)(&line);
        }
    };
    let exit_outcome: Result<EngineOutcome, String> = loop {
        drain_pct(&pct_rx);
        drain_lines(&line_rx);
        if (hooks.cancelled)() {
            // Cancel-during-completion race: a child that already exited
            // keeps its real outcome — a completed run is never
            // discarded, and a real failure is never masked as a cancel.
            if let Ok(Some(status)) = child.try_wait() {
                if status.success() {
                    break Ok(EngineOutcome::Success);
                }
                break Ok(EngineOutcome::Failed {
                    exit_code: status.code(),
                    tail: String::new(), // populated below from the drained buffer
                });
            }
            kill_child_group(&mut child);
            break Ok(EngineOutcome::Cancelled);
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                if status.success() {
                    break Ok(EngineOutcome::Success);
                }
                break Ok(EngineOutcome::Failed {
                    exit_code: status.code(),
                    tail: String::new(),
                });
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(100)),
            Err(e) => break Err(e.to_string()),
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
        Ok(EngineOutcome::Failed { exit_code, .. }) => {
            // stderr leads (that's where Python tracebacks land); stdout
            // follows so installer errors reported there aren't lost.
            let err = stderr_buf
                .lock()
                .map(|b| output_tail(&b))
                .unwrap_or_default();
            let out = stdout_buf
                .lock()
                .map(|b| output_tail(&b))
                .unwrap_or_default();
            let tail = match (err.is_empty(), out.is_empty()) {
                (false, false) => format!("{err}\n{out}"),
                (true, false) => out,
                _ => err,
            };
            Ok(EngineOutcome::Failed { exit_code, tail })
        }
        other => other,
    }
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
/// by [`run_engine_process`] — a process-wide registry because the
/// children are spawned deep inside the lib crate while the quit
/// decision happens in the TUI binary.
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

/// Extract the most recent percentage tick from a chunk of engine stderr.
///
/// tqdm renders `\r`-rewritten lines shaped like
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
        let mut end = i;
        let mut start = i;
        while start > 0 && end - start < 3 && bytes[start - 1].is_ascii_digit() {
            start -= 1;
        }
        if start == i {
            continue; // '%' with no digits before it
        }
        // Decimal percentage ("45.6%"): the digits right before '%' are
        // the fraction — shift the window to the integer part instead of
        // reporting the fractional digits as their own tick (45.6% used
        // to parse as 6% and rewind the progress strip).
        if start > 0 && bytes[start - 1] == b'.' {
            end = start - 1;
            start = end;
            while start > 0 && end - start < 3 && bytes[start - 1].is_ascii_digit() {
                start -= 1;
            }
            if start == end {
                continue; // ".6%" — no integer part, not a percentage
            }
        }
        if start > 0 && bytes[start - 1].is_ascii_digit() {
            // A digit run longer than three ("1000%") isn't a percentage;
            // taking its last three digits would report a bogus tick.
            continue;
        }
        if let Ok(pct) = chunk[start..end].parse::<u16>() {
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

/// Trim a captured pipe buffer down to the interesting tail for error
/// reporting: tqdm progress lines (anything carrying a `%|` bar) are
/// dropped, `\r` rewrites are treated as line breaks, and only the last
/// few remaining lines are kept. Applied to both pipes on failure.
fn output_tail(raw: &[u8]) -> String {
    let text = String::from_utf8_lossy(raw);
    let lines: Vec<&str> = text
        .split(['\n', '\r'])
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.contains("%|"))
        .collect();
    let start = lines.len().saturating_sub(8);
    lines[start..].join("\n")
}

/// `true` when `path` is a runnable engine binary: a regular file with
/// any execute bit on unix; any regular file elsewhere.
pub(crate) fn is_executable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    fn temp_root(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "zytunes-engineproc-{tag}-{}-{}",
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

    /// Collected hook output from a full driver run.
    #[cfg(unix)]
    struct Run {
        outcome: Result<EngineOutcome, String>,
        lines: Vec<String>,
        ticks: Vec<u8>,
    }

    /// Run `sh -c script` through the driver with permissive defaults.
    #[cfg(unix)]
    fn drive_sh(script: &str, cancelled: &dyn Fn() -> bool) -> Run {
        let _lock = lock_driver_tests();
        let lines = std::sync::Mutex::new(Vec::new());
        let ticks = std::sync::Mutex::new(Vec::new());
        let on_line = |l: &str| lines.lock().unwrap().push(l.to_string());
        let on_progress = |p: u8| ticks.lock().unwrap().push(p);
        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-c").arg(script);
        let outcome = run_engine_process(
            &mut cmd,
            &EngineHooks {
                cancelled,
                on_progress: Some(&on_progress),
                on_line: &on_line,
            },
        );
        Run {
            outcome,
            lines: lines.into_inner().unwrap(),
            ticks: ticks.into_inner().unwrap(),
        }
    }

    #[test]
    #[cfg(unix)]
    fn driver_success_streams_lines_and_progress() {
        let run = drive_sh(
            "echo out-line; printf ' 42%%|##        | bar\\r' 1>&2; echo err-line 1>&2",
            &|| false,
        );
        assert_eq!(run.outcome, Ok(EngineOutcome::Success));
        assert!(run.lines.iter().any(|l| l == "out-line"), "{:?}", run.lines);
        assert!(run.lines.iter().any(|l| l == "err-line"), "{:?}", run.lines);
        assert!(
            !run.lines.iter().any(|l| l.contains("%|")),
            "progress bars go through the percent channel, not the log: {:?}",
            run.lines
        );
        assert!(run.ticks.contains(&42), "{:?}", run.ticks);
    }

    #[test]
    #[cfg(unix)]
    fn driver_failure_reports_exit_code_and_stderr_tail() {
        let run = drive_sh("echo boom 1>&2; exit 3", &|| false);
        match run.outcome {
            Ok(EngineOutcome::Failed { exit_code, tail }) => {
                assert_eq!(exit_code, Some(3));
                assert!(tail.contains("boom"), "tail: {tail}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    #[cfg(unix)]
    fn driver_failure_tail_includes_stdout_diagnostics() {
        // Installers (the curl|sh uv bootstrap, uv itself) report errors
        // on stdout; a stderr-only tail surfaced those as a bare exit
        // code. stderr still leads when both pipes carried content.
        let run = drive_sh("echo 'resolution failed: no torch' ; exit 2", &|| false);
        match run.outcome {
            Ok(EngineOutcome::Failed { exit_code, tail }) => {
                assert_eq!(exit_code, Some(2));
                assert!(tail.contains("resolution failed"), "tail: {tail}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }

        let run = drive_sh("echo out-detail; echo err-cause 1>&2; exit 2", &|| false);
        match run.outcome {
            Ok(EngineOutcome::Failed { tail, .. }) => {
                let err_pos = tail
                    .find("err-cause")
                    .unwrap_or_else(|| panic!("missing err-cause in tail: {tail:?}"));
                let out_pos = tail
                    .find("out-detail")
                    .unwrap_or_else(|| panic!("missing out-detail in tail: {tail:?}"));
                assert!(err_pos < out_pos, "stderr should lead: {tail}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    #[cfg(unix)]
    fn driver_spawn_error_is_err_not_outcome() {
        let mut cmd = std::process::Command::new("/nonexistent/zytunes-engine-test");
        let err = run_engine_process(
            &mut cmd,
            &EngineHooks {
                cancelled: &|| false,
                on_progress: None,
                on_line: &|_| {},
            },
        );
        assert!(err.is_err());
    }

    #[test]
    #[cfg(unix)]
    fn driver_cancel_kills_pipeline_grandchildren() {
        // The uv bootstrap is `sh -c "curl … | sh"`: killing only the
        // direct sh leaves the pipeline stages running (the original
        // cancel bug). Model the shape with a shell that spawns a
        // long-lived grandchild and reports its pid; cancel once the
        // grandchild exists and assert the whole group died.
        let root = temp_root("cancel-grandchild");
        let pidfile = root.join("grandchild.pid");
        let pidfile_probe = pidfile.clone();
        let cancelled =
            move || std::fs::read_to_string(&pidfile_probe).is_ok_and(|s| !s.trim().is_empty());
        let run = drive_sh(
            &format!("sleep 30 & echo $! > {}; wait", pidfile.to_string_lossy()),
            &cancelled,
        );
        assert_eq!(run.outcome, Ok(EngineOutcome::Cancelled));
        let grandchild: libc::pid_t = std::fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .parse()
            .expect("pid parses");
        wait_until("grandchild death", || !pid_alive(grandchild));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    #[cfg(unix)]
    fn driver_cancel_race_preserves_real_failure() {
        // A child that already failed by the time the cancel poll trips
        // must report its real failure, not a masking Cancelled (the
        // masked variant hid genuine install failures behind a quiet
        // "cancelled" log line).
        let first_poll = AtomicBool::new(true);
        let cancelled = || !first_poll.swap(false, Ordering::SeqCst);
        let run = drive_sh("exit 5", &cancelled);
        match run.outcome {
            Ok(EngineOutcome::Failed { exit_code, .. }) => assert_eq!(exit_code, Some(5)),
            other => panic!("expected Failed(5), got {other:?}"),
        }
    }

    #[test]
    #[cfg(unix)]
    fn driver_cancel_race_preserves_success() {
        let first_poll = AtomicBool::new(true);
        let cancelled = || !first_poll.swap(false, Ordering::SeqCst);
        let run = drive_sh("exit 0", &cancelled);
        assert_eq!(run.outcome, Ok(EngineOutcome::Success));
    }

    #[test]
    #[cfg(unix)]
    fn driver_skips_percent_parsing_without_progress_hook() {
        let _lock = lock_driver_tests();
        // on_progress: None must not panic on tqdm-shaped stderr and
        // still delivers info lines.
        let calls = AtomicUsize::new(0);
        let on_line = |_: &str| {
            calls.fetch_add(1, Ordering::SeqCst);
        };
        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-c")
            .arg("printf ' 42%%|bar\\r' 1>&2; echo info 1>&2");
        let outcome = run_engine_process(
            &mut cmd,
            &EngineHooks {
                cancelled: &|| false,
                on_progress: None,
                on_line: &on_line,
            },
        );
        assert_eq!(outcome, Ok(EngineOutcome::Success));
        assert_eq!(calls.load(Ordering::SeqCst), 1, "only the info line");
    }

    #[test]
    #[cfg(unix)]
    fn kill_child_group_takes_down_pipeline_grandchildren() {
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
        let _ = std::fs::remove_dir_all(&root);
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
        let _lock = lock_driver_tests();
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
        // Decimal percentages report their integer part — the fraction
        // alone ("45.6%" → 6) rewound the strip. No integer part → no
        // tick; oversized integer parts stay rejected.
        assert_eq!(parse_progress_percent("45.6%|"), Some(45));
        assert_eq!(parse_progress_percent("99.99%|"), Some(99));
        assert_eq!(parse_progress_percent(".6%|"), None);
        assert_eq!(parse_progress_percent("2050.5%|"), None);
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
    fn output_tail_drops_progress_bars_and_keeps_diagnostics() {
        let raw = b" 10%|#         | x\r 20%|##        | y\nTraceback (most recent call last):\n  ValueError: bad model\n";
        let tail = output_tail(raw);
        assert!(tail.contains("ValueError: bad model"));
        assert!(!tail.contains("%|"));
    }
}
