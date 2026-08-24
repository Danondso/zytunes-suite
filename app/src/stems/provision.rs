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

/// audio-separator release the managed install pins. The argv contract
/// (`--custom_output_names` JSON, `--model_file_dir`, stem-name keys)
/// is verified against this exact version — bump deliberately, never
/// let it float.
pub const AUDIO_SEPARATOR_VERSION: &str = "0.44.3";

/// A stem-engine executable zytunes knows how to drive. Which engine a
/// recipe needs comes from [`crate::stems::RecipeKind::engine`]; this
/// enum owns the executable name and install spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineKind {
    /// The `demucs` CLI (or a compatible fork — both upstream and
    /// `demucs-next` expose a `demucs` entry point).
    Demucs,
    /// `python-audio-separator`'s `audio-separator` CLI, which runs
    /// Roformer checkpoints and demucs models alike.
    AudioSeparator,
}

impl EngineKind {
    /// Executable name the engine package installs (and discovery
    /// searches for).
    pub fn exe_name(self) -> &'static str {
        match self {
            EngineKind::Demucs => "demucs",
            EngineKind::AudioSeparator => "audio-separator",
        }
    }

    /// uv package spec installed when `[stems] package` is unset.
    /// demucs is unpinned (upstream is frozen; nothing can drift);
    /// audio-separator is version-pinned so its CLI contract can't
    /// change under us, with the extra selecting the onnxruntime
    /// flavor — torch itself is a core dependency either way, which is
    /// why the CPU index pin in [`build_install_command`] applies to
    /// both engines.
    pub fn default_package(self, gpu: bool) -> String {
        match self {
            EngineKind::Demucs => "demucs".to_string(),
            EngineKind::AudioSeparator => format!(
                "audio-separator[{}]=={AUDIO_SEPARATOR_VERSION}",
                if gpu { "gpu" } else { "cpu" }
            ),
        }
    }
}

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

/// A ready-to-spawn subprocess invocation: `(program, args, envs)` —
/// the shape every `build_*_command` constructor here returns.
pub type CommandSpec = (PathBuf, Vec<String>, Vec<(String, String)>);

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

use super::process::is_executable;

/// Locate a usable binary for `engine` without installing anything.
///
/// Precedence: explicit config `command` (if it points at a real file
/// AND its file name matches `engine`) → the engine's exe on `path_env`
/// → `bin_dir/<exe>` (the managed-install location). A configured
/// command that doesn't exist falls through to the automatic candidates
/// rather than erroring — the config may simply predate a venv rebuild.
///
/// The engine match matters because `[stems] command` is one shared
/// config field written back by whichever engine last installed:
/// honoring a demucs path while the hq recipe asks for audio-separator
/// would drive the wrong binary with the wrong argv AND suppress the
/// install consent prompt for the engine actually needed.
pub fn find_engine_with(
    engine: EngineKind,
    explicit: Option<&Path>,
    path_env: &str,
    bin_dir: Option<&Path>,
) -> Option<PathBuf> {
    discover_engine_with(engine, explicit, path_env, bin_dir).map(|(p, _)| p)
}

/// Which precedence rung engine discovery resolved a binary from. Shown
/// by the stem settings panel so "why am I not being prompted to
/// install" answers itself — a stale `[stems] command` is visibly the
/// rung in charge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineSource {
    /// The explicit `[stems] command` config value.
    ConfigCommand,
    /// Found as the engine's exe name on `PATH`.
    PathEnv,
    /// zytunes' managed install dir (`~/.local/share/zytunes/bin`).
    Managed,
}

impl EngineSource {
    /// Short human-readable rung name for the settings panel.
    pub fn label(self) -> &'static str {
        match self {
            EngineSource::ConfigCommand => "[stems] command",
            EngineSource::PathEnv => "PATH",
            EngineSource::Managed => "managed install",
        }
    }
}

/// [`find_engine_with`] that also reports which precedence rung matched.
pub fn discover_engine_with(
    engine: EngineKind,
    explicit: Option<&Path>,
    path_env: &str,
    bin_dir: Option<&Path>,
) -> Option<(PathBuf, EngineSource)> {
    if let Some(cmd) = explicit {
        if is_executable(cmd) && command_matches_engine(cmd, engine) {
            return Some((cmd.to_path_buf(), EngineSource::ConfigCommand));
        }
    }
    if let Some(found) = find_on_path_in(engine.exe_name(), path_env) {
        return Some((found, EngineSource::PathEnv));
    }
    bin_dir
        .map(|d| d.join(engine.exe_name()))
        .filter(|p| is_executable(p))
        .map(|p| (p, EngineSource::Managed))
}

/// [`discover_engine_with`] against the real environment.
pub fn discover_engine(
    engine: EngineKind,
    explicit: Option<&Path>,
) -> Option<(PathBuf, EngineSource)> {
    let path_env = std::env::var("PATH").unwrap_or_default();
    discover_engine_with(engine, explicit, &path_env, zytunes_bin_dir().as_deref())
}

/// Build the `uv tool uninstall` invocation for an engine package spec.
/// uv addresses installed tools by bare package name, never by the
/// versioned/extra'd spec that installed them.
pub fn build_uninstall_command(
    uv: &Path,
    package_spec: &str,
    bin_dir: Option<&Path>,
) -> Result<CommandSpec, String> {
    let name = package_name(package_spec);
    // `uv tool uninstall` is destructive and the spec is user config: a
    // name uv would parse as a flag (`--all` removes every uv tool on
    // the machine) must never reach argv.
    if name.is_empty() || name.starts_with('-') {
        return Err(format!(
            "refusing to uninstall suspicious package name {name:?} from [stems] package"
        ));
    }
    // Same UV_TOOL_BIN_DIR pin as the install: uv resolves the shim dir
    // at run time, so an unpinned uninstall could leave a stale shim in
    // the managed bin dir that discovery keeps "finding".
    let envs = bin_dir
        .map(|d| {
            vec![(
                "UV_TOOL_BIN_DIR".to_string(),
                d.to_string_lossy().into_owned(),
            )]
        })
        .unwrap_or_default();
    Ok((
        uv.to_path_buf(),
        vec![
            "tool".to_string(),
            "uninstall".to_string(),
            name.to_string(),
        ],
        envs,
    ))
}

/// Whether an explicit `[stems] command` plausibly belongs to `engine`:
/// its file name must contain the engine's exe name (`demucs`,
/// `demucs-next`, `my-demucs` all match Demucs; none match
/// AudioSeparator). Contains rather than equality so fork shims and
/// wrapper scripts keep working; the paths the provisioner writes back
/// always end in the exact exe name.
fn command_matches_engine(cmd: &Path, engine: EngineKind) -> bool {
    cmd.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.contains(engine.exe_name()))
}

/// [`find_engine_with`] against the real environment.
pub fn find_engine(engine: EngineKind, explicit: Option<&Path>) -> Option<PathBuf> {
    let path_env = std::env::var("PATH").unwrap_or_default();
    find_engine_with(engine, explicit, &path_env, zytunes_bin_dir().as_deref())
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
    engine: EngineKind,
    package: &str,
    gpu: bool,
    bin_dir: &Path,
) -> (PathBuf, Vec<String>, Vec<(String, String)>) {
    let mut args: Vec<String> = vec!["tool".into(), "install".into(), package.into()];
    if engine == EngineKind::Demucs {
        // demucs' package metadata omits numpy — it historically arrived
        // transitively via torch, which stopped depending on it — so the
        // env imports `numpy` at runtime and dies with ModuleNotFoundError
        // unless it's injected explicitly. audio-separator declares its
        // own `numpy>=2`; injecting there would just fight it.
        args.extend(["--with".into(), "numpy".into()]);
    }
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

/// Where `engine`'s shim lands after a managed install.
pub fn managed_engine_path(bin_dir: &Path, engine: EngineKind) -> PathBuf {
    bin_dir.join(engine.exe_name())
}

/// Run `program args` with `envs`, streaming every output line (stdout
/// and stderr, `\r` treated as a line break) to `on_line`, polling
/// `cancelled` every ~100 ms and killing the child when it trips. A thin
/// [`ProvisionError`] mapping over the shared engine-process driver.
pub fn run_streaming(
    program: &Path,
    args: &[String],
    envs: &[(String, String)],
    cancelled: &dyn Fn() -> bool,
    on_line: &dyn Fn(&str),
) -> Result<(), ProvisionError> {
    use super::process::{run_engine_process, EngineHooks, EngineOutcome};

    let mut cmd = std::process::Command::new(program);
    cmd.args(args)
        .envs(envs.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    let hooks = EngineHooks {
        cancelled,
        // Installers print no tqdm bars; skip percent parsing.
        on_progress: None,
        on_line,
    };
    match run_engine_process(&mut cmd, &hooks) {
        Err(e) => Err(ProvisionError::Spawn(format!("{}: {e}", program.display()))),
        Ok(EngineOutcome::Success) => Ok(()),
        Ok(EngineOutcome::Cancelled) => Err(ProvisionError::Cancelled),
        Ok(EngineOutcome::Failed { exit_code, tail }) => Err(ProvisionError::Failed(format!(
            "exit {exit_code:?}\n{tail}"
        ))),
    }
}

/// Full managed provisioning: ensure `uv` (bootstrapping it after the
/// caller has obtained user consent), install the engine package, and
/// return the path of the installed engine binary.
pub fn provision_engine(
    engine: EngineKind,
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
    let (prog, args, envs) = build_install_command(&uv, engine, package, gpu, &bin_dir);
    run_streaming(&prog, &args, &envs, cancelled, on_line)?;

    let installed = managed_engine_path(&bin_dir, engine);
    if installed.is_file() {
        Ok(installed)
    } else {
        Err(ProvisionError::EngineMissingAfterInstall(installed))
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

    #[cfg(unix)]
    #[test]
    fn discover_engine_reports_the_precedence_rung() {
        let root = temp_dir("discover");
        let cfg_dir = root.join("cfg");
        let path_dir = root.join("path");
        let managed = root.join("managed");
        for d in [&cfg_dir, &path_dir, &managed] {
            std::fs::create_dir_all(d).unwrap();
        }
        let explicit = write_executable(&cfg_dir, "demucs");
        let on_path = write_executable(&path_dir, "demucs");
        let shim = write_executable(&managed, "demucs");
        let path_env = path_dir.to_string_lossy().into_owned();

        // Explicit config command wins and is labelled as such.
        let (p, src) = discover_engine_with(
            EngineKind::Demucs,
            Some(&explicit),
            &path_env,
            Some(&managed),
        )
        .unwrap();
        assert_eq!(p, explicit);
        assert_eq!(src, EngineSource::ConfigCommand);

        // No explicit → PATH.
        let (p, src) =
            discover_engine_with(EngineKind::Demucs, None, &path_env, Some(&managed)).unwrap();
        assert_eq!(p, on_path);
        assert_eq!(src, EngineSource::PathEnv);

        // Nothing on PATH → the managed install dir.
        let (p, src) = discover_engine_with(EngineKind::Demucs, None, "", Some(&managed)).unwrap();
        assert_eq!(p, shim);
        assert_eq!(src, EngineSource::Managed);

        assert!(discover_engine_with(EngineKind::Demucs, None, "", None).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn build_uninstall_command_uses_the_bare_package_name() {
        let (prog, args, envs) = build_uninstall_command(
            Path::new("/u/uv"),
            &format!("audio-separator[cpu]=={AUDIO_SEPARATOR_VERSION}"),
            Some(Path::new("/managed/bin")),
        )
        .unwrap();
        assert_eq!(prog, Path::new("/u/uv"));
        assert_eq!(args, vec!["tool", "uninstall", "audio-separator"]);
        assert_eq!(
            envs,
            vec![("UV_TOOL_BIN_DIR".to_string(), "/managed/bin".to_string())],
            "uninstall pins the shim dir like the install does"
        );
        let (_, args, envs) = build_uninstall_command(Path::new("/u/uv"), "demucs", None).unwrap();
        assert_eq!(args, vec!["tool", "uninstall", "demucs"]);
        assert!(envs.is_empty());
    }

    #[test]
    fn build_uninstall_command_rejects_flag_shaped_names() {
        // `uv tool uninstall --all` removes every uv tool on the machine;
        // a config value must never be able to smuggle a flag into argv.
        for spec in ["--all", "-q", "", "  "] {
            assert!(
                build_uninstall_command(Path::new("/u/uv"), spec, None).is_err(),
                "{spec:?} must be refused"
            );
        }
    }

    #[test]
    fn engine_exe_names_and_default_packages() {
        assert_eq!(EngineKind::Demucs.exe_name(), "demucs");
        assert_eq!(EngineKind::AudioSeparator.exe_name(), "audio-separator");

        // demucs installs unpinned (upstream is frozen; nothing drifts).
        assert_eq!(EngineKind::Demucs.default_package(false), "demucs");
        assert_eq!(EngineKind::Demucs.default_package(true), "demucs");
        // audio-separator is version-pinned so the argv/JSON contract
        // can't drift under us, and the extra picks the onnxruntime
        // flavor (torch itself is a core dep either way).
        assert_eq!(
            EngineKind::AudioSeparator.default_package(false),
            format!("audio-separator[cpu]=={AUDIO_SEPARATOR_VERSION}")
        );
        assert_eq!(
            EngineKind::AudioSeparator.default_package(true),
            format!("audio-separator[gpu]=={AUDIO_SEPARATOR_VERSION}")
        );
    }

    #[test]
    fn install_command_injects_numpy_only_for_demucs() {
        // demucs' package metadata omits numpy (it historically arrived
        // transitively via torch); audio-separator declares numpy>=2
        // properly, and an injected constraint would just fight it.
        let (_p, demucs_args, _e) = build_install_command(
            Path::new("uv"),
            EngineKind::Demucs,
            "demucs",
            false,
            Path::new("/b"),
        );
        assert!(demucs_args
            .windows(2)
            .any(|w| w[0] == "--with" && w[1] == "numpy"));

        let (_p, sep_args, _e) = build_install_command(
            Path::new("uv"),
            EngineKind::AudioSeparator,
            "audio-separator[cpu]==1.0",
            false,
            Path::new("/b"),
        );
        assert!(
            !sep_args.iter().any(|a| a == "numpy"),
            "audio-separator declares its own numpy: {sep_args:?}"
        );
    }

    #[test]
    fn install_command_pins_cpu_torch_for_both_engines() {
        // torch is a core dependency of BOTH engines; without the CPU
        // index pin a Linux install pulls the multi-GB CUDA build.
        for engine in [EngineKind::Demucs, EngineKind::AudioSeparator] {
            let (_p, args, _e) = build_install_command(
                Path::new("uv"),
                engine,
                &engine.default_package(false),
                false,
                Path::new("/b"),
            );
            let idx = args
                .iter()
                .position(|a| a == "--index")
                .unwrap_or_else(|| panic!("{engine:?}: --index missing in {args:?}"));
            assert_eq!(args[idx + 1], TORCH_CPU_INDEX);
            assert!(args
                .windows(2)
                .any(|w| w[0] == "--index-strategy" && w[1] == "unsafe-best-match"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn find_engine_locates_the_requested_engine_only() {
        let bin_dir = temp_dir("engine-kind");
        let _demucs = write_executable(&bin_dir, "demucs");
        // Only demucs is installed: the audio-separator lookup must NOT
        // fall back to it.
        assert!(find_engine_with(EngineKind::Demucs, None, "", Some(&bin_dir)).is_some());
        assert!(find_engine_with(EngineKind::AudioSeparator, None, "", Some(&bin_dir)).is_none());

        let sep = write_executable(&bin_dir, "audio-separator");
        assert_eq!(
            find_engine_with(EngineKind::AudioSeparator, None, "", Some(&bin_dir)),
            Some(sep)
        );
        let _ = std::fs::remove_dir_all(&bin_dir);
    }

    #[cfg(unix)]
    #[test]
    fn explicit_command_only_matches_its_own_engine() {
        // `[stems] command` is one shared field: a demucs path written
        // back by a demucs install must not satisfy an audio-separator
        // lookup after a recipe switch (wrong argv + suppressed install
        // consent), and vice versa.
        let bin_dir = temp_dir("explicit-engine");
        let demucs = write_executable(&bin_dir, "demucs");
        assert_eq!(
            find_engine_with(EngineKind::Demucs, Some(&demucs), "", None),
            Some(demucs.clone())
        );
        assert_eq!(
            find_engine_with(EngineKind::AudioSeparator, Some(&demucs), "", None),
            None
        );
        // Mismatched explicit still falls through to the managed dir.
        let sep = write_executable(&bin_dir, "audio-separator");
        assert_eq!(
            find_engine_with(
                EngineKind::AudioSeparator,
                Some(&demucs),
                "",
                Some(&bin_dir)
            ),
            Some(sep)
        );
        // Fork shims keep working: contains match, not equality.
        let fork = write_executable(&bin_dir, "demucs-next");
        assert_eq!(
            find_engine_with(EngineKind::Demucs, Some(&fork), "", None),
            Some(fork)
        );
        let _ = std::fs::remove_dir_all(&bin_dir);
    }

    #[test]
    fn managed_engine_path_is_per_engine() {
        assert_eq!(
            managed_engine_path(Path::new("/b/bin"), EngineKind::Demucs),
            Path::new("/b/bin/demucs")
        );
        assert_eq!(
            managed_engine_path(Path::new("/b/bin"), EngineKind::AudioSeparator),
            Path::new("/b/bin/audio-separator")
        );
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
        assert_eq!(
            find_engine_with(EngineKind::Demucs, None, &path_env, Some(&bin_dir)),
            None
        );

        // Managed bin dir only.
        let managed = write_executable(&bin_dir, "demucs");
        assert_eq!(
            find_engine_with(EngineKind::Demucs, None, &path_env, Some(&bin_dir)),
            Some(managed.clone())
        );

        // PATH beats bin dir.
        let on_path = write_executable(&path_dir, "demucs");
        assert_eq!(
            find_engine_with(EngineKind::Demucs, None, &path_env, Some(&bin_dir)),
            Some(on_path.clone())
        );

        // Explicit beats both…
        let explicit = write_executable(&explicit_dir, "my-demucs");
        assert_eq!(
            find_engine_with(
                EngineKind::Demucs,
                Some(&explicit),
                &path_env,
                Some(&bin_dir)
            ),
            Some(explicit.clone())
        );

        // …but a dangling explicit path falls through to PATH.
        let dangling = explicit_dir.join("gone");
        assert_eq!(
            find_engine_with(
                EngineKind::Demucs,
                Some(&dangling),
                &path_env,
                Some(&bin_dir)
            ),
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
            EngineKind::Demucs,
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
        let (_prog, args, envs) = build_install_command(
            Path::new("uv"),
            EngineKind::Demucs,
            "demucs",
            true,
            Path::new("/b"),
        );
        assert!(
            !args
                .iter()
                .any(|a| a == "--index" || a.contains("pytorch.org")),
            "gpu installs resolve torch from the default (CUDA/MPS-capable) index"
        );
        assert!(envs.iter().any(|(k, _)| k == "UV_TOOL_BIN_DIR"));
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
