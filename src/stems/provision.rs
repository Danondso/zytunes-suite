//! Stem-engine provisioning: find a demucs binary, or install one via `uv`.
//!
//! Nothing here runs at startup — the background worker calls
//! [`find_engine`] on the first `M`-press and only reaches for
//! [`provision_engine`] after the user consents in the TUI overlay.
//!
//! `uv` is the managed-install vehicle because it is a single static
//! binary that creates isolated tool environments and can bootstrap
//! Python itself, and its `UV_TOOL_BIN_DIR` env var lets us pin the
//! installed `demucs` shim into zytunes' own bin dir
//! (`~/.local/share/zytunes/bin`) so discovery never depends on the
//! user's PATH configuration. CPU-only installs pin PyTorch's CPU wheel
//! index (~1.5 GB installed instead of the multi-GB CUDA stack); see
//! `docs/stem-splitting-plan.md` §2.

use std::path::{Path, PathBuf};

/// Executable name the engine package installs. Both upstream `demucs`
/// and the `demucs-next` fork expose a `demucs` entry point, so this is
/// package-independent today; revisit if a future fork renames it.
pub const ENGINE_EXE: &str = "demucs";

/// PyTorch's CPU-only wheel index — hosts `torch` builds without the
/// bundled CUDA runtime.
pub const TORCH_CPU_INDEX: &str = "https://download.pytorch.org/whl/cpu";

/// Astral's official uv installer script. Piped through `sh` with
/// `UV_INSTALL_DIR` pointing at zytunes' bin dir and PATH modification
/// disabled — the standalone binary is all we want.
pub const UV_INSTALL_SCRIPT_URL: &str = "https://astral.sh/uv/install.sh";

#[derive(Debug)]
pub enum ProvisionError {
    /// A subprocess could not be spawned (missing binary, permissions).
    Spawn(String),
    /// A subprocess exited non-zero; the string carries its output tail.
    Failed(String),
    /// User cancelled mid-install.
    Cancelled,
    /// The install finished but no engine binary appeared where expected.
    EngineMissingAfterInstall(PathBuf),
    /// No `$HOME`, so nowhere to put the managed install.
    NoHome,
}

impl std::fmt::Display for ProvisionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProvisionError::Spawn(e) => write!(f, "failed to spawn installer: {e}"),
            ProvisionError::Failed(tail) => write!(f, "engine install failed: {tail}"),
            ProvisionError::Cancelled => write!(f, "engine install cancelled by user"),
            ProvisionError::EngineMissingAfterInstall(p) => write!(
                f,
                "install finished but no engine binary at {}",
                p.display()
            ),
            ProvisionError::NoHome => write!(f, "cannot resolve $HOME for the managed install"),
        }
    }
}

impl std::error::Error for ProvisionError {}

/// zytunes' private bin dir for managed executables (the bootstrapped
/// `uv` and the `demucs` shim uv installs there via `UV_TOOL_BIN_DIR`).
pub fn zytunes_bin_dir() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(
        Path::new(&home)
            .join(".local")
            .join("share")
            .join("zytunes")
            .join("bin"),
    )
}

/// Normalize a pip requirement spec down to its bare package name:
/// `demucs-next==1.2` → `demucs-next`, `demucs[extra]>=4` → `demucs`.
/// Used for log messages and the uv tools-dir fallback path.
pub fn package_name(spec: &str) -> &str {
    let spec = spec.trim();
    let end = spec
        .find(['=', '<', '>', '[', '@', ' ', '!', '~'])
        .unwrap_or(spec.len());
    &spec[..end]
}

/// Search `path_env` (a `PATH`-formatted string) for an executable named
/// `name`. Unix executability (any x bit) is required; on non-unix any
/// regular file matches. Splitting goes through `std::env::split_paths`
/// so the platform's separator convention applies instead of a
/// hard-coded `':'`.
pub fn find_on_path_in(name: &str, path_env: &str) -> Option<PathBuf> {
    std::env::split_paths(path_env)
        .filter(|d| !d.as_os_str().is_empty())
        .map(|d| d.join(name))
        .find(|p| is_executable(p))
}

fn is_executable(path: &Path) -> bool {
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

/// Locate a usable engine binary without installing anything.
///
/// Precedence: explicit config `command` (if it points at a real file) →
/// `demucs` on `path_env` → `bin_dir/demucs` (the managed-install
/// location). A configured command that doesn't exist falls through to
/// the automatic candidates rather than erroring — the config may simply
/// predate a venv rebuild.
pub fn find_engine_with(
    explicit: Option<&Path>,
    path_env: &str,
    bin_dir: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(cmd) = explicit {
        if is_executable(cmd) {
            return Some(cmd.to_path_buf());
        }
    }
    if let Some(found) = find_on_path_in(ENGINE_EXE, path_env) {
        return Some(found);
    }
    bin_dir
        .map(|d| d.join(ENGINE_EXE))
        .filter(|p| is_executable(p))
}

/// [`find_engine_with`] against the real environment.
pub fn find_engine(explicit: Option<&Path>) -> Option<PathBuf> {
    let path_env = std::env::var("PATH").unwrap_or_default();
    find_engine_with(explicit, &path_env, zytunes_bin_dir().as_deref())
}

/// Locate a usable `uv` binary: PATH first, then the bootstrapped copy in
/// [`zytunes_bin_dir`].
pub fn find_uv_with(path_env: &str, bin_dir: Option<&Path>) -> Option<PathBuf> {
    if let Some(found) = find_on_path_in("uv", path_env) {
        return Some(found);
    }
    bin_dir.map(|d| d.join("uv")).filter(|p| is_executable(p))
}

/// [`find_uv_with`] against the real environment.
pub fn find_uv() -> Option<PathBuf> {
    let path_env = std::env::var("PATH").unwrap_or_default();
    find_uv_with(&path_env, zytunes_bin_dir().as_deref())
}

/// Build the `uv tool install` invocation for the engine package.
///
/// Returns `(program, args, envs)`. CPU installs (`gpu = false`) pin
/// PyTorch's CPU index as an extra index with the cross-index
/// `unsafe-best-match` strategy so `torch` resolves from the CPU index
/// while everything else comes from PyPI. `UV_TOOL_BIN_DIR` pins the
/// installed shim into `bin_dir` so [`find_engine`] can find it without
/// PATH help.
pub fn build_install_command(
    uv: &Path,
    package: &str,
    gpu: bool,
    bin_dir: &Path,
) -> (PathBuf, Vec<String>, Vec<(String, String)>) {
    let mut args: Vec<String> = vec![
        "tool".into(),
        "install".into(),
        package.into(),
        // demucs' package metadata omits numpy — it historically arrived
        // transitively via torch, which stopped depending on it — so the
        // env imports `numpy` at runtime and dies with ModuleNotFoundError
        // unless it's injected explicitly.
        "--with".into(),
        "numpy".into(),
    ];
    if !gpu {
        args.extend([
            "--index".into(),
            TORCH_CPU_INDEX.into(),
            "--index-strategy".into(),
            "unsafe-best-match".into(),
        ]);
    }
    let envs = vec![(
        "UV_TOOL_BIN_DIR".to_string(),
        bin_dir.to_string_lossy().into_owned(),
    )];
    (uv.to_path_buf(), args, envs)
}

/// Build the uv bootstrap invocation: Astral's install script piped
/// through `sh`, with `UV_INSTALL_DIR` pointing at `bin_dir` and PATH
/// modification disabled. Returns `(program, args, envs)`.
pub fn build_uv_bootstrap_command(bin_dir: &Path) -> (PathBuf, Vec<String>, Vec<(String, String)>) {
    let script = format!("curl -LsSf {UV_INSTALL_SCRIPT_URL} | sh");
    let envs = vec![
        (
            "UV_INSTALL_DIR".to_string(),
            bin_dir.to_string_lossy().into_owned(),
        ),
        ("UV_NO_MODIFY_PATH".to_string(), "1".to_string()),
    ];
    (PathBuf::from("sh"), vec!["-c".into(), script], envs)
}

/// Where the engine shim lands after a managed install.
pub fn managed_engine_path(bin_dir: &Path) -> PathBuf {
    bin_dir.join(ENGINE_EXE)
}

/// Run `program args` with `envs`, streaming every output line (stdout
/// and stderr, `\r` treated as a line break) to `on_line`, polling
/// `cancelled` every ~100 ms and killing the child when it trips.
///
/// Not unit-tested against a live child (the `rip_track_cancellable`
/// precedent) — exercised end-to-end via the manual provisioning flow.
pub fn run_streaming(
    program: &Path,
    args: &[String],
    envs: &[(String, String)],
    cancelled: &dyn Fn() -> bool,
    on_line: &dyn Fn(&str),
) -> Result<(), ProvisionError> {
    use std::sync::{Arc, Mutex};

    let mut cmd = std::process::Command::new(program);
    cmd.args(args)
        .envs(envs.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    // An installer orphaned by TUI death would keep holding uv's own
    // locks — same failure mode as an orphaned engine. The process group
    // matters doubly here: the uv bootstrap is a `sh -c "curl … | sh"`
    // pipeline whose stages are grandchildren a direct kill would miss.
    super::isolate_child_process(&mut cmd);
    let mut child = cmd
        .spawn()
        .map_err(|e| ProvisionError::Spawn(format!("{}: {e}", program.display())))?;
    let _group = super::ChildGroupGuard::register(&child);

    // Both pipes drain continuously (deadlock avoidance) into a shared
    // line channel; a bounded tail is kept for the error message.
    let (line_tx, line_rx) = std::sync::mpsc::channel::<String>();
    let tail: Arc<Mutex<std::collections::VecDeque<String>>> =
        Arc::new(Mutex::new(std::collections::VecDeque::new()));
    let mut handles = Vec::new();
    for pipe in [
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
    ]
    .into_iter()
    .flatten()
    {
        let tx = line_tx.clone();
        let tail = Arc::clone(&tail);
        handles.push(std::thread::spawn(move || {
            let mut reader = pipe;
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        buf.extend_from_slice(&chunk[..n]);
                        // Emit complete lines; \r counts as a terminator so
                        // progress rewrites surface as they happen.
                        while let Some(pos) = buf.iter().position(|b| *b == b'\n' || *b == b'\r') {
                            let line: Vec<u8> = buf.drain(..=pos).collect();
                            let text = String::from_utf8_lossy(&line[..line.len() - 1])
                                .trim()
                                .to_string();
                            if !text.is_empty() {
                                if let Ok(mut t) = tail.lock() {
                                    t.push_back(text.clone());
                                    while t.len() > 12 {
                                        t.pop_front();
                                    }
                                }
                                let _ = tx.send(text);
                            }
                        }
                    }
                }
            }
        }));
    }
    drop(line_tx);

    let drain = |rx: &std::sync::mpsc::Receiver<String>| {
        while let Ok(line) = rx.try_recv() {
            on_line(&line);
        }
    };
    let outcome: Result<(), ProvisionError> = loop {
        drain(&line_rx);
        if cancelled() {
            // Cancel-during-completion race: a child that already exited
            // keeps its real outcome — success stays success, and a
            // failure must NOT be masked as a user cancel (the masked
            // variant hid genuine install failures behind a quiet
            // "cancelled" log line).
            if let Ok(Some(status)) = child.try_wait() {
                if status.success() {
                    break Ok(());
                }
                break Err(ProvisionError::Failed(format!("exit {:?}", status.code())));
            }
            super::kill_child_group(&mut child);
            break Err(ProvisionError::Cancelled);
        }
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break Ok(()),
            Ok(Some(status)) => {
                break Err(ProvisionError::Failed(format!("exit {:?}", status.code())))
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(100)),
            Err(e) => break Err(ProvisionError::Spawn(e.to_string())),
        }
    };
    for h in handles {
        let _ = h.join();
    }
    drain(&line_rx);

    match outcome {
        Err(ProvisionError::Failed(code)) => {
            let tail_text = tail
                .lock()
                .map(|t| t.iter().cloned().collect::<Vec<_>>().join("\n"))
                .unwrap_or_default();
            Err(ProvisionError::Failed(format!("{code}\n{tail_text}")))
        }
        other => other,
    }
}

/// Full managed provisioning: ensure `uv` (bootstrapping it after the
/// caller has obtained user consent), install the engine package, and
/// return the path of the installed engine binary.
pub fn provision_engine(
    package: &str,
    gpu: bool,
    cancelled: &dyn Fn() -> bool,
    on_line: &dyn Fn(&str),
) -> Result<PathBuf, ProvisionError> {
    let bin_dir = zytunes_bin_dir().ok_or(ProvisionError::NoHome)?;
    std::fs::create_dir_all(&bin_dir).map_err(|e| ProvisionError::Spawn(e.to_string()))?;

    let uv = match find_uv() {
        Some(uv) => uv,
        None => {
            on_line("uv not found — fetching the standalone uv binary…");
            let (prog, args, envs) = build_uv_bootstrap_command(&bin_dir);
            run_streaming(&prog, &args, &envs, cancelled, on_line)?;
            find_uv()
                .ok_or_else(|| ProvisionError::EngineMissingAfterInstall(bin_dir.join("uv")))?
        }
    };

    on_line(&format!(
        "installing {} via uv (one-time)…",
        package_name(package)
    ));
    let (prog, args, envs) = build_install_command(&uv, package, gpu, &bin_dir);
    run_streaming(&prog, &args, &envs, cancelled, on_line)?;

    let engine = managed_engine_path(&bin_dir);
    if engine.is_file() {
        Ok(engine)
    } else {
        Err(ProvisionError::EngineMissingAfterInstall(engine))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "zytunes-provision-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[cfg(unix)]
    fn write_executable(dir: &Path, name: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join(name);
        std::fs::write(&p, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[test]
    fn package_name_strips_version_and_extras() {
        assert_eq!(package_name("demucs"), "demucs");
        assert_eq!(package_name("demucs-next==1.2"), "demucs-next");
        assert_eq!(package_name("demucs[extra]>=4"), "demucs");
        assert_eq!(package_name("demucs @ git+https://x"), "demucs");
        assert_eq!(package_name("  demucs<5  "), "demucs");
    }

    #[cfg(unix)]
    #[test]
    fn find_on_path_requires_executable_bit() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("path-exec");
        let exe = write_executable(&dir, "demucs");
        let path_env = dir.to_string_lossy().into_owned();
        assert_eq!(find_on_path_in("demucs", &path_env), Some(exe.clone()));

        // Strip the x bits → no longer found.
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(find_on_path_in("demucs", &path_env), None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn find_on_path_scans_entries_in_order() {
        let a = temp_dir("path-a");
        let b = temp_dir("path-b");
        let in_b = write_executable(&b, "demucs");
        let path_env = format!("{}:{}", a.display(), b.display());
        // Only b has it.
        assert_eq!(find_on_path_in("demucs", &path_env), Some(in_b));
        // Once a has it too, a wins (PATH order).
        let in_a = write_executable(&a, "demucs");
        assert_eq!(find_on_path_in("demucs", &path_env), Some(in_a));

        let _ = std::fs::remove_dir_all(&a);
        let _ = std::fs::remove_dir_all(&b);
    }

    #[cfg(unix)]
    #[test]
    fn find_engine_precedence_explicit_then_path_then_bin_dir() {
        let explicit_dir = temp_dir("engine-explicit");
        let path_dir = temp_dir("engine-path");
        let bin_dir = temp_dir("engine-bin");
        let path_env = path_dir.to_string_lossy().into_owned();

        // Nothing anywhere → None.
        assert_eq!(find_engine_with(None, &path_env, Some(&bin_dir)), None);

        // Managed bin dir only.
        let managed = write_executable(&bin_dir, "demucs");
        assert_eq!(
            find_engine_with(None, &path_env, Some(&bin_dir)),
            Some(managed.clone())
        );

        // PATH beats bin dir.
        let on_path = write_executable(&path_dir, "demucs");
        assert_eq!(
            find_engine_with(None, &path_env, Some(&bin_dir)),
            Some(on_path.clone())
        );

        // Explicit beats both…
        let explicit = write_executable(&explicit_dir, "my-demucs");
        assert_eq!(
            find_engine_with(Some(&explicit), &path_env, Some(&bin_dir)),
            Some(explicit.clone())
        );

        // …but a dangling explicit path falls through to PATH.
        let dangling = explicit_dir.join("gone");
        assert_eq!(
            find_engine_with(Some(&dangling), &path_env, Some(&bin_dir)),
            Some(on_path)
        );

        let _ = std::fs::remove_dir_all(&explicit_dir);
        let _ = std::fs::remove_dir_all(&path_dir);
        let _ = std::fs::remove_dir_all(&bin_dir);
    }

    #[test]
    fn install_command_cpu_pins_torch_cpu_index() {
        let (prog, args, envs) = build_install_command(
            Path::new("/usr/bin/uv"),
            "demucs",
            false,
            Path::new("/home/u/.local/share/zytunes/bin"),
        );
        assert_eq!(prog, Path::new("/usr/bin/uv"));
        assert_eq!(args[..3], ["tool", "install", "demucs"]);
        let idx = args.iter().position(|a| a == "--index").expect("--index");
        assert_eq!(args[idx + 1], TORCH_CPU_INDEX);
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--index-strategy" && w[1] == "unsafe-best-match"),
            "cross-index resolution needed so only torch comes from the CPU index"
        );
        assert!(envs
            .iter()
            .any(|(k, v)| k == "UV_TOOL_BIN_DIR" && v == "/home/u/.local/share/zytunes/bin"));
    }

    #[test]
    fn install_command_gpu_uses_default_index() {
        let (_prog, args, envs) =
            build_install_command(Path::new("uv"), "demucs", true, Path::new("/b"));
        assert!(
            !args
                .iter()
                .any(|a| a == "--index" || a.contains("pytorch.org")),
            "gpu installs resolve torch from the default (CUDA/MPS-capable) index"
        );
        assert!(envs.iter().any(|(k, _)| k == "UV_TOOL_BIN_DIR"));
    }

    #[test]
    fn install_command_always_injects_numpy() {
        // demucs' package metadata omits numpy (it historically arrived
        // transitively via torch, which no longer depends on it), so the
        // tool env crashes with ModuleNotFoundError at separation time
        // without this. Both index modes need it.
        for gpu in [false, true] {
            let (_prog, args, _envs) =
                build_install_command(Path::new("uv"), "demucs", gpu, Path::new("/b"));
            assert!(
                args.windows(2).any(|w| w[0] == "--with" && w[1] == "numpy"),
                "gpu={gpu}: expected `--with numpy` in {args:?}"
            );
        }
    }

    #[test]
    fn bootstrap_command_pipes_installer_through_sh() {
        let (prog, args, envs) = build_uv_bootstrap_command(Path::new("/b/bin"));
        assert_eq!(prog, Path::new("sh"));
        assert_eq!(args[0], "-c");
        assert!(args[1].contains(UV_INSTALL_SCRIPT_URL));
        assert!(args[1].contains("curl"));
        assert!(envs
            .iter()
            .any(|(k, v)| k == "UV_INSTALL_DIR" && v == "/b/bin"));
        assert!(envs
            .iter()
            .any(|(k, v)| k == "UV_NO_MODIFY_PATH" && v == "1"));
    }

    #[test]
    fn managed_engine_path_joins_exe_name() {
        assert_eq!(
            managed_engine_path(Path::new("/b/bin")),
            Path::new("/b/bin/demucs")
        );
    }

    #[cfg(unix)]
    #[test]
    fn find_uv_prefers_path_then_managed_dir() {
        let path_dir = temp_dir("uv-path");
        let bin_dir = temp_dir("uv-bin");
        let path_env = path_dir.to_string_lossy().into_owned();

        assert_eq!(find_uv_with(&path_env, Some(&bin_dir)), None);
        let managed = write_executable(&bin_dir, "uv");
        assert_eq!(find_uv_with(&path_env, Some(&bin_dir)), Some(managed));
        let on_path = write_executable(&path_dir, "uv");
        assert_eq!(find_uv_with(&path_env, Some(&bin_dir)), Some(on_path));

        let _ = std::fs::remove_dir_all(&path_dir);
        let _ = std::fs::remove_dir_all(&bin_dir);
    }
}
