use crate::mtp::parse::{self, DeviceEntry};
use crate::mtp::DeviceSession;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

/// Strip ANSI escape codes from a string.
/// aft-mtp-cli prepends cursor-movement codes (e.g. \x1b[1A\x1b[2K) to output lines.
fn strip_ansi(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Skip \x1b[...X sequences (where X is a letter).
            if chars.peek() == Some(&'[') {
                chars.next(); // consume '['
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            // Non-CSI escape: just drop the ESC, keep the next char.
        } else {
            result.push(c);
        }
    }
    result
}

/// Escape a string for safe use as a quoted argument in aft-mtp-cli commands.
/// Escapes double quotes, strips newlines/carriage returns, and wraps in double quotes.
fn aft_quote(s: &str) -> String {
    let sanitized: String = s
        .chars()
        .filter(|c| *c != '\n' && *c != '\r')
        .collect::<String>()
        .replace('"', "\\\"");
    format!("\"{sanitized}\"")
}

/// Parse a progress line from aft-mtp-cli output.
/// Format: `:progress <filename> <transferred> <total>`
fn parse_progress(line: &str) -> Option<(u64, u64)> {
    if !line.starts_with(":progress") {
        return None;
    }
    let parts: Vec<&str> = line.rsplitn(3, ' ').collect();
    if parts.len() < 2 {
        return None;
    }
    let total: u64 = parts[0].parse().ok()?;
    let transferred: u64 = parts[1].parse().ok()?;
    Some((transferred, total))
}

/// Check that the MTPZ key file has secure permissions (owner-only).
#[cfg(unix)]
fn check_mtpz_permissions(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let metadata =
        std::fs::metadata(path).map_err(|e| format!("Cannot stat {}: {e}", path.display()))?;
    let mode = metadata.permissions().mode();
    if mode & 0o077 != 0 {
        Err(format!(
            "{} has mode {:o} — should be 600 (owner-only). Fix with: chmod 600 {}",
            path.display(),
            mode & 0o777,
            path.display()
        ))
    } else {
        Ok(())
    }
}

/// Build the path to the MTPZ data file.
fn mtpz_data_path() -> Result<PathBuf, String> {
    let home =
        std::env::var("HOME").map_err(|_| "HOME environment variable is not set".to_string())?;
    Ok(PathBuf::from(home).join(".mtpz-data"))
}

/// An MTP session backed by aft-mtp-cli subprocess.
/// Handles MTPZ authentication automatically on connect.
pub struct AftSession {
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
    log_tx: Option<std::sync::mpsc::Sender<String>>,
}

impl AftSession {
    /// Set a channel for log messages. When set, all progress and status
    /// messages are sent through this channel instead of printing to stderr.
    pub fn set_log_sender(&mut self, tx: std::sync::mpsc::Sender<String>) {
        self.log_tx = Some(tx);
    }

    /// Log a message — sends to channel if set, otherwise prints to stderr.
    fn log(&self, msg: &str) {
        if let Some(ref tx) = self.log_tx {
            let clean = msg.trim_start_matches('\r').trim();
            if !clean.is_empty() {
                let _ = tx.send(clean.to_string());
            }
        } else {
            eprint!("{}", msg);
        }
    }

    /// Log a message with a newline.
    fn logln(&self, msg: &str) {
        if let Some(ref tx) = self.log_tx {
            let clean = msg.trim_start_matches('\r').trim();
            if !clean.is_empty() {
                let _ = tx.send(clean.to_string());
            }
        } else {
            eprintln!("{}", msg);
        }
    }

    /// Find the aft-mtp-cli binary.
    fn find_binary() -> Result<PathBuf, String> {
        // 1. AFT_MTP_CLI env var.
        if let Ok(path) = std::env::var("AFT_MTP_CLI") {
            let p = PathBuf::from(&path);
            if p.exists() {
                return Ok(p);
            }
            return Err(format!("AFT_MTP_CLI={path} does not exist"));
        }

        // 2. Local build in aft/ subdirectory (monorepo).
        if let Ok(exe) = std::env::current_exe() {
            if let Some(project_dir) = exe
                .parent()
                .and_then(|p| p.parent())
                .and_then(|p| p.parent())
            {
                let local = project_dir.join("aft/build/cli/aft-mtp-cli");
                if local.exists() {
                    return Ok(local);
                }
            }
        }
        // Also check relative to cwd.
        let local_cwd = PathBuf::from("aft/build/cli/aft-mtp-cli");
        if local_cwd.exists() {
            return Ok(std::fs::canonicalize(&local_cwd).unwrap_or(local_cwd));
        }

        // 3. Check PATH.
        if let Ok(output) = Command::new("which").arg("aft-mtp-cli").output() {
            if output.status.success() {
                let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !path.is_empty() {
                    return Ok(PathBuf::from(path));
                }
            }
        }

        // 4. Common build location.
        let fallback = PathBuf::from("/tmp/aft/build/cli/aft-mtp-cli");
        if fallback.exists() {
            return Ok(fallback);
        }

        Err(
            "Could not find aft-mtp-cli. Run: cd aft && mkdir build && cd build && \
             cmake .. -DBUILD_QT_UI=OFF -DBUILD_MTPZ=ON -DBUILD_FUSE=OFF && make aft-mtp-cli"
                .to_string(),
        )
    }

    /// Open a session to the Zune via aft-mtp-cli.
    /// This spawns the subprocess, which automatically performs MTPZ authentication.
    pub fn open() -> Result<Self, String> {
        // Verify .mtpz-data exists.
        let mtpz_path = mtpz_data_path()?;
        if !mtpz_path.exists() {
            return Err(format!(
                "MTPZ keys not found at {}\n\
                 Copy mtpz-data.example to ~/.mtpz-data",
                mtpz_path.display()
            ));
        }

        #[cfg(unix)]
        if let Err(warning) = check_mtpz_permissions(&mtpz_path) {
            // Logged after session is constructed so log_tx can be used.
            // For now just print — this runs before the TUI takes over.
            eprintln!("Warning: {warning}");
        }

        let binary = Self::find_binary()?;

        // Spawn with -b (batch) and -e (events — prints `:done` after each command).
        let mut child = Command::new(&binary)
            .args(["-b", "-e"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Failed to spawn {}: {e}", binary.display()))?;

        let stdin = child.stdin.take().ok_or("No stdin pipe")?;
        let stdout = child.stdout.take().ok_or("No stdout pipe")?;
        let reader = BufReader::new(stdout);

        // Drain stderr in a background thread to prevent pipe deadlock.
        if let Some(stderr) = child.stderr.take() {
            std::thread::spawn(move || {
                let reader = BufReader::new(stderr);
                for _line in reader.lines().map_while(Result::ok) {}
            });
        }

        let mut session = AftSession {
            child,
            stdin,
            reader,
            log_tx: None,
        };

        // Wait for startup. aft-mtp-cli prints device/storage info (no `:done` marker).
        // Read lines until we see "selected storage" which means auth succeeded.
        let startup = session.read_until_startup(Duration::from_secs(15))?;
        for line in &startup {
            if line.contains("failed") || line.contains("error") {
                return Err(format!("aft-mtp-cli startup failed: {line}"));
            }
        }

        Ok(session)
    }

    /// Query MTP device info: manufacturer, model, firmware version, serial number.
    /// Returns raw lines from the `device-info` command.
    pub fn device_info(&mut self) -> Result<Vec<String>, String> {
        self.send("device-info")
    }

    /// Query storage info for a given storage path/id.
    /// Returns a line like: "used 12345678 (45%), free 15000000 bytes of 27345678"
    pub fn storage_info(&mut self, storage: &str) -> Result<Vec<String>, String> {
        self.send(&format!("storage-info {}", aft_quote(storage)))
    }

    /// Send a command and collect output lines until `:done`.
    pub fn send(&mut self, cmd: &str) -> Result<Vec<String>, String> {
        self.send_with_timeout(cmd, Duration::from_secs(30))
    }

    /// Send a command with a custom timeout.
    pub fn send_with_timeout(
        &mut self,
        cmd: &str,
        timeout: Duration,
    ) -> Result<Vec<String>, String> {
        writeln!(self.stdin, "{cmd}").map_err(|e| format!("Write to aft-mtp-cli: {e}"))?;
        self.stdin
            .flush()
            .map_err(|e| format!("Flush aft-mtp-cli: {e}"))?;
        self.read_until_done(timeout)
    }

    /// Read startup lines until we see "selected storage" (auth complete).
    fn read_until_startup(&mut self, timeout: Duration) -> Result<Vec<String>, String> {
        let start = Instant::now();
        let mut lines = Vec::new();
        let mut buf = String::new();

        loop {
            if start.elapsed() > timeout {
                return Err("Timeout waiting for aft-mtp-cli startup".to_string());
            }
            buf.clear();
            match self.reader.read_line(&mut buf) {
                Ok(0) => return Err("aft-mtp-cli exited during startup".to_string()),
                Ok(_) => {
                    let raw = buf.trim_end_matches('\n').trim_end_matches('\r');
                    let line = strip_ansi(raw);
                    lines.push(line.to_string());
                    if line.contains("selected storage") {
                        return Ok(lines);
                    }
                }
                Err(e) => return Err(format!("Read from aft-mtp-cli: {e}")),
            }
        }
    }

    /// Read lines from stdout until we see `:done` or timeout.
    fn read_until_done(&mut self, timeout: Duration) -> Result<Vec<String>, String> {
        self.read_until_done_inner(timeout, false)
    }

    /// Read lines until `:done`, optionally printing progress to stderr.
    fn read_until_done_inner(
        &mut self,
        timeout: Duration,
        verbose: bool,
    ) -> Result<Vec<String>, String> {
        let start = Instant::now();
        let mut lines = Vec::new();
        let mut buf = String::new();

        loop {
            if start.elapsed() > timeout {
                return Err(format!(
                    "Timeout waiting for aft-mtp-cli response ({}s)",
                    timeout.as_secs()
                ));
            }

            buf.clear();
            match self.reader.read_line(&mut buf) {
                Ok(0) => {
                    return Err("aft-mtp-cli process exited unexpectedly".to_string());
                }
                Ok(_) => {
                    let raw = buf.trim_end_matches('\n').trim_end_matches('\r');
                    let line = strip_ansi(raw);
                    let line = line.trim();
                    if line == ":done" {
                        return Ok(lines);
                    }
                    if verbose {
                        if let Some((transferred, total)) = parse_progress(line) {
                            let pct = (transferred as f64 / total as f64 * 100.0) as u32;
                            let mb = transferred as f64 / 1_048_576.0;
                            let total_mb = total as f64 / 1_048_576.0;
                            self.log(&format!("\r  Uploading: {:.1}/{:.1} MB ({}%)    ", mb, total_mb, pct));
                        } else if line.starts_with("album:")
                            || line.starts_with("getting ")
                            || line.starts_with("artists ")
                            || line.starts_with("albums ")
                            || line.starts_with("music ")
                            || line.starts_with("device ")
                            || line.starts_with("abstract ")
                        {
                            self.log(&format!("\r  {}    ", &line[..line.len().min(70)]));
                        }
                    }
                    lines.push(line.to_string());
                }
                Err(e) => {
                    return Err(format!("Read from aft-mtp-cli: {e}"));
                }
            }
        }
    }
}

impl DeviceSession for AftSession {
    /// List directory contents (extended format with object IDs, formats, sizes).
    fn ls(&mut self, path: &str) -> Result<Vec<DeviceEntry>, String> {
        self.send(&format!("cd {}", aft_quote(path)))?;
        let lines = self.send("lsext")?;
        Ok(parse::parse_lsext(&lines))
    }

    /// Import an audio file into the Zune library with proper metadata.
    /// Returns the device object ID of the created track.
    fn zune_import(&mut self, local_path: &str) -> Result<u64, String> {
        writeln!(self.stdin, "zune-import {}", aft_quote(local_path))
            .map_err(|e| format!("Write: {e}"))?;
        self.stdin.flush().map_err(|e| format!("Flush: {e}"))?;

        let timeout = Duration::from_secs(900); // 15 min — library load is very slow
        let start = Instant::now();
        let mut buf = String::new();
        let mut album_count = 0u32;
        let mut track_id: Option<u64> = None;

        loop {
            if start.elapsed() > timeout {
                return Err("zune-import timed out (15 min)".to_string());
            }
            buf.clear();
            match self.reader.read_line(&mut buf) {
                Ok(0) => return Err("aft-mtp-cli exited during import".to_string()),
                Ok(_) => {
                    let raw = buf.trim();
                    let line = strip_ansi(raw);
                    let line = line.trim();
                    if line == ":done" {
                        self.log(&format!("\r{}\r", " ".repeat(60)));
                        return track_id.ok_or_else(|| {
                            "import succeeded but no track-id received".to_string()
                        });
                    }
                    if let Some(id_str) = line.strip_prefix("track-id:") {
                        track_id = id_str.trim().parse::<u64>().ok();
                    } else if line.starts_with("album:") {
                        album_count += 1;
                        if album_count.is_multiple_of(10) || album_count <= 5 {
                            self.log(&format!("  Loading library: {} albums indexed...", album_count));
                        }
                    } else if line.starts_with("getting ") {
                        self.log(&format!("  {}...", line));
                    } else if let Some((transferred, total)) = parse_progress(line) {
                        let pct = (transferred as f64 / total as f64 * 100.0) as u32;
                        let mb = transferred as f64 / 1_048_576.0;
                        let total_mb = total as f64 / 1_048_576.0;
                        self.log(&format!("  Uploading: {:.1}/{:.1} MB ({}%)", mb, total_mb, pct));
                    } else if line.contains("error:") {
                        self.logln(&format!("  {}", line));
                        return Err(line.to_string());
                    } else if line.starts_with("device ")
                        || line.starts_with("abstract ")
                        || line.starts_with("artists ")
                        || line.starts_with("albums ")
                        || line.starts_with("music ")
                    {
                        self.log(&format!("  {}", &line[..line.len().min(55)]));
                    }
                }
                Err(e) => return Err(format!("Read: {e}")),
            }
        }
    }

    /// Remove a file or directory from the device.
    /// Deletes leaf-first (the Zune rejects deleting non-empty folders).
    fn rm(&mut self, device_path: &str) -> Result<(), String> {
        // Find the object by listing its parent directory.
        let (parent, name) = match device_path.rfind('/') {
            Some(pos) if pos > 0 => (&device_path[..pos], &device_path[pos + 1..]),
            _ => ("/", device_path.trim_start_matches('/')),
        };
        self.send(&format!("cd {}", aft_quote(parent)))?;
        let parent_lines = self.send("lsext")?;
        let parent_entries = parse::parse_lsext(&parent_lines);
        let target = parent_entries.iter().find(|e| e.name.ends_with(name));

        if let Some(entry) = target {
            if entry.is_dir() {
                // Directory: list recursively and delete children first.
                let lines = self.send(&format!("lsext-r {}", aft_quote(device_path)))?;
                let entries = parse::parse_lsext(&lines);

                let files: Vec<u64> = entries
                    .iter()
                    .filter(|e| !e.is_dir())
                    .map(|e| e.object_id)
                    .collect();
                let mut dirs: Vec<u64> = entries
                    .iter()
                    .filter(|e| e.is_dir())
                    .map(|e| e.object_id)
                    .collect();
                dirs.reverse(); // deepest first

                for id in files.iter().chain(dirs.iter()) {
                    let lines = self.send(&format!("rm-id {id}"))?;
                    for line in &lines {
                        if line.contains("error") {
                            self.logln(&format!("  Warning: rm-id {id}: {line}"));
                        }
                    }
                }
            }

            // Delete the target itself by object ID.
            let rm_lines = self.send(&format!("rm-id {}", entry.object_id))?;
            for line in &rm_lines {
                if line.contains("error") {
                    return Err(line.clone());
                }
            }
            Ok(())
        } else {
            Err(format!("Could not find \"{}\" in \"{}\"", name, parent))
        }
    }

    /// Collect all non-directory entries under a path, recursively.
    fn collect_all_tracks(&mut self, path: &str) -> Result<Vec<DeviceEntry>, String> {
        let lines = self.send_with_timeout(
            &format!("lsext-r {}", aft_quote(path)),
            Duration::from_secs(120),
        )?;
        let entries = parse::parse_lsext(&lines);
        Ok(entries.into_iter().filter(|e| !e.is_dir()).collect())
    }

    /// Create a playlist on the device with the given track object IDs.
    fn create_playlist(&mut self, name: &str, track_ids: &[u64]) -> Result<(), String> {
        let ids_str: Vec<String> = track_ids.iter().map(|id| id.to_string()).collect();
        let cmd = format!(
            "create-playlist {} \"{}\"",
            aft_quote(name),
            ids_str.join(",")
        );
        let lines = self.send(&cmd)?;
        for line in &lines {
            if line.contains("error") {
                return Err(line.clone());
            }
        }
        Ok(())
    }
}

impl Drop for AftSession {
    fn drop(&mut self) {
        let _ = writeln!(self.stdin, "quit");
        let _ = self.stdin.flush();
        // Give the process a chance to exit cleanly before killing.
        for _ in 0..10 {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                Err(_) => break,
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
impl AftSession {
    /// Construct from an existing child process (for testing Drop behavior).
    fn from_child(mut child: Child) -> Self {
        let stdin = child.stdin.take().expect("child must have stdin");
        let stdout = child.stdout.take().expect("child must have stdout");
        let reader = BufReader::new(stdout);
        AftSession {
            child,
            stdin,
            reader,
            log_tx: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_ansi_removes_escape_sequences() {
        assert_eq!(strip_ansi(""), "");
        assert_eq!(strip_ansi("Hello World"), "Hello World");
        assert_eq!(strip_ansi("\x1b[1A\x1b[2KHello"), "Hello");
        assert_eq!(strip_ansi("\x1b[1Afoo\x1b[2Kbar\x1b[0m"), "foobar");
        // Non-CSI escape: ESC not followed by '[' should only drop ESC.
        assert_eq!(strip_ansi("\x1bXHello"), "XHello");
        // Bare ESC at end of string.
        assert_eq!(strip_ansi("foo\x1b"), "foo");
    }

    #[test]
    fn aft_quote_sanitizes_dangerous_characters() {
        assert_eq!(aft_quote(r#"foo"bar"#), r#""foo\"bar""#);
        assert_eq!(aft_quote("foo\nbar"), r#""foobar""#);
        assert_eq!(aft_quote("foo\rbar"), r#""foobar""#);
    }

    #[test]
    fn aft_quote_wraps_normal_input() {
        assert_eq!(aft_quote("/Music/Artist/Album"), r#""/Music/Artist/Album""#);
        assert_eq!(aft_quote("My Album Name"), r#""My Album Name""#);
    }

    #[test]
    fn parse_progress_valid_and_invalid() {
        let (transferred, total) = parse_progress(":progress song.mp3 524288 1048576").unwrap();
        assert_eq!(transferred, 524288);
        assert_eq!(total, 1048576);

        assert!(parse_progress("not a progress line").is_none());
        assert!(parse_progress(":progress").is_none());
    }

    #[test]
    #[cfg(unix)]
    fn check_mtpz_permissions_enforces_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join("zune-test-perms");
        let _ = std::fs::create_dir_all(&dir);

        let world_readable = dir.join("mtpz-test-644");
        std::fs::write(&world_readable, "test").unwrap();
        std::fs::set_permissions(&world_readable, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(check_mtpz_permissions(&world_readable).is_err());

        let owner_only = dir.join("mtpz-test-600");
        std::fs::write(&owner_only, "test").unwrap();
        std::fs::set_permissions(&owner_only, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(check_mtpz_permissions(&owner_only).is_ok());

        let _ = std::fs::remove_file(&world_readable);
        let _ = std::fs::remove_file(&owner_only);
    }

    #[test]
    fn mtpz_data_path_returns_path_in_home() {
        if std::env::var("HOME").is_ok() {
            let path = mtpz_data_path().unwrap();
            assert!(path.to_str().unwrap().ends_with(".mtpz-data"));
        }
    }

    #[test]
    fn drop_does_not_panic_if_child_already_exited() {
        let child = Command::new("echo")
            .arg("hello")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let session = AftSession::from_child(child);
        std::thread::sleep(Duration::from_millis(100));
        drop(session);
    }
}
