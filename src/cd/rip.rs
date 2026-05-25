//! CD audio ripping via `ffmpeg` shell-out.
//!
//! Shelling out avoids pulling in `libcdio`/`libcdio-paranoia` as a second
//! native dependency on top of the disc-id work, and reuses the `ffmpeg`
//! binary the existing video-sync flow already depends on. Documented in
//! CLAUDE.md alongside the video-sync mention.
//!
//! Phase 0 builds the command-construction surface and validates it via
//! pure-string tests. Phase 3 wires this into the background worker and
//! parses ffmpeg's progress lines off stderr.

use std::path::{Path, PathBuf};

use super::discid::DiscToc;

/// Output format / quality target for a ripped track.
///
/// FLAC, ALAC, and WAV land in the library at full source quality. On push
/// to a device they're transcoded to the highest fidelity that device
/// supports (ALAC for iPod Classic via the FLAC→ALAC ffmpeg branch; LAME
/// VBR NearBest (~V0) for Zune since the Zune firmware has no lossless
/// container) — see `DeviceCapabilities::lossless_target`. ALAC and AAC
/// both produce `.m4a` files; iPod consumes them as passthrough, Zune
/// falls back to MP3 transcode since its firmware doesn't read MP4-
/// containerised audio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RipFidelity {
    /// MP3 CBR 320 kbps.
    Mp3Cbr320,
    /// MP3 VBR ~245 kbps (LAME `-V 0` equivalent).
    Mp3V0,
    /// MP3 VBR ~190 kbps (LAME `-V 2` equivalent).
    Mp3V2,
    /// AAC CBR 256 kbps (iTunes Plus standard). Native ffmpeg encoder —
    /// no external `libfdk_aac` dependency. Output container is `.m4a`.
    Aac,
    /// Apple Lossless. Output container is `.m4a`. Native ffmpeg encoder.
    /// Passthrough on iPod push; transcoded to MP3 on Zune push.
    Alac,
    /// Lossless FLAC. Stored in the library and transcoded on device push.
    Flac,
    /// Lossless WAV passthrough. Largest files; useful for archival or
    /// before-encoder inspection.
    Wav,
}

impl RipFidelity {
    /// File extension (lowercase, no leading dot) the rip will land as.
    pub fn extension(self) -> &'static str {
        match self {
            RipFidelity::Mp3Cbr320 | RipFidelity::Mp3V0 | RipFidelity::Mp3V2 => "mp3",
            RipFidelity::Aac | RipFidelity::Alac => "m4a",
            RipFidelity::Flac => "flac",
            RipFidelity::Wav => "wav",
        }
    }

    /// Short human-readable label for the picker UI.
    pub fn label(self) -> &'static str {
        match self {
            RipFidelity::Mp3Cbr320 => "MP3 320 kbps CBR",
            RipFidelity::Mp3V0 => "MP3 V0 (~245 kbps VBR)",
            RipFidelity::Mp3V2 => "MP3 V2 (~190 kbps VBR)",
            RipFidelity::Aac => "AAC 256 kbps CBR (iTunes Plus)",
            RipFidelity::Alac => "ALAC (Apple Lossless)",
            RipFidelity::Flac => "FLAC (lossless)",
            RipFidelity::Wav => "WAV (lossless, uncompressed)",
        }
    }

    /// Default order for the picker — picking-order matches storage size,
    /// ascending. Library users tend to want lossless when ripping.
    pub fn all() -> &'static [RipFidelity] {
        &[
            RipFidelity::Mp3V2,
            RipFidelity::Mp3V0,
            RipFidelity::Aac,
            RipFidelity::Mp3Cbr320,
            RipFidelity::Alac,
            RipFidelity::Flac,
            RipFidelity::Wav,
        ]
    }

    /// One-line summary of the tags that get embedded in the ripped file
    /// for this fidelity. Today the set is uniform across all variants
    /// (`tag_ripped_file` writes the same `ItemKey` items regardless of
    /// container, and lofty handles the per-container encoding) so every
    /// arm returns the same `RIP_TAG_SUMMARY` constant. The per-variant
    /// dispatch exists so future container-specific tag drops (e.g. an
    /// M4A field lofty doesn't round-trip) can be flagged here without
    /// changing the caller. Surfaced in the TUI import overlay.
    pub fn tag_summary(self) -> &'static str {
        match self {
            RipFidelity::Mp3Cbr320
            | RipFidelity::Mp3V0
            | RipFidelity::Mp3V2
            | RipFidelity::Aac
            | RipFidelity::Alac
            | RipFidelity::Flac
            | RipFidelity::Wav => crate::cd::metadata::RIP_TAG_SUMMARY,
        }
    }
}

/// Progress update emitted during a rip. Phase 3 turns these into
/// `BgEvent::RipProgress` messages.
#[derive(Debug, Clone, PartialEq)]
pub struct RipProgress {
    pub track_number: u8,
    /// 0..=100. May be `None` early in the rip before the duration is known.
    pub percent: Option<u8>,
}

/// Build the ffmpeg command line for ripping a single track from a CD via
/// the `libcdio` input demuxer.
///
/// Returns the program name and argv tail so callers can spawn it or inspect
/// it for tests. `drive_device` should be a node ffmpeg can open via the
/// `libcdio` input — typically `/dev/srN` on Linux.
///
/// **macOS note:** stock Homebrew ffmpeg ships without `--enable-libcdio`,
/// so this command would fail with "Unknown input format: 'libcdio'".
/// The macOS path goes through [`build_ffmpeg_command_from_file`] instead,
/// reading the AIFF files macOS auto-mounts under `/Volumes/<title>/`.
/// [`build_command_for_track`] dispatches between the two.
pub fn build_ffmpeg_command(
    drive_device: &Path,
    track_number: u8,
    fidelity: RipFidelity,
    output: &Path,
) -> (PathBuf, Vec<String>) {
    let mut args: Vec<String> = vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "warning".into(),
        // Always overwrite — the worker chooses the destination path.
        "-y".into(),
        // Stream key=value progress lines to stdout so `rip_track_cancellable`
        // can render per-track progress in the TUI. Doesn't slow the rip.
        "-progress".into(),
        "pipe:1".into(),
        // `libcdio` demuxer reads audio CDs track-addressably.
        "-f".into(),
        "libcdio".into(),
        "-i".into(),
        drive_device.to_string_lossy().into_owned(),
        // ffmpeg's libcdio input exposes each track as a separate input stream;
        // -map selects the requested track (0-indexed in ffmpeg).
        "-map".into(),
        format!("0:{}", track_number.saturating_sub(1)),
    ];

    extend_codec_args(&mut args, fidelity);

    args.push(output.to_string_lossy().into_owned());
    (PathBuf::from("ffmpeg"), args)
}

/// Append the codec / quality / output-format switch for `fidelity` onto
/// `args`. Shared between [`build_ffmpeg_command`] (libcdio input) and
/// [`build_ffmpeg_command_from_file`] (file input) so a fidelity change
/// can't drift between the two.
///
/// The `-f <muxer>` flag is essential because the rip pipeline writes to
/// a temp filename ending in `.part` (e.g. `track.m4a.part`) — without
/// `-f`, ffmpeg's filename-based muxer autodetection sees `.part`,
/// fails to recognise it, and aborts with "Unable to choose an output
/// format". The `.part` suffix is preserved on the filename so the
/// dirlib scanner doesn't index partial files mid-rip.
fn extend_codec_args(args: &mut Vec<String>, fidelity: RipFidelity) {
    match fidelity {
        RipFidelity::Mp3Cbr320 => {
            args.extend([
                "-codec:a".into(),
                "libmp3lame".into(),
                "-b:a".into(),
                "320k".into(),
                "-f".into(),
                "mp3".into(),
            ]);
        }
        RipFidelity::Mp3V0 => {
            args.extend([
                "-codec:a".into(),
                "libmp3lame".into(),
                "-q:a".into(),
                "0".into(),
                "-f".into(),
                "mp3".into(),
            ]);
        }
        RipFidelity::Mp3V2 => {
            args.extend([
                "-codec:a".into(),
                "libmp3lame".into(),
                "-q:a".into(),
                "2".into(),
                "-f".into(),
                "mp3".into(),
            ]);
        }
        RipFidelity::Aac => {
            // ffmpeg's built-in native `aac` encoder (no external libfdk_aac
            // dependency). 256k CBR matches the iTunes Plus standard a lot
            // of listeners will recognise as "transparent". Output muxer
            // is `ipod` — same MP4-with-iTunes-tweaks container Picard and
            // Apple Music produce.
            args.extend([
                "-codec:a".into(),
                "aac".into(),
                "-b:a".into(),
                "256k".into(),
                "-f".into(),
                "ipod".into(),
            ]);
        }
        RipFidelity::Alac => {
            // Native ALAC encoder — same encoder the FLAC→ALAC iPod push
            // branch uses. Bit-exact lossless; the bitrate falls out of
            // the source signal. `-f ipod` for the same reason as AAC.
            args.extend(["-codec:a".into(), "alac".into(), "-f".into(), "ipod".into()]);
        }
        RipFidelity::Flac => {
            args.extend(["-codec:a".into(), "flac".into(), "-f".into(), "flac".into()]);
        }
        RipFidelity::Wav => {
            args.extend([
                "-codec:a".into(),
                "pcm_s16le".into(),
                "-f".into(),
                "wav".into(),
            ]);
        }
    }
}

/// Build the ffmpeg command line for ripping a single track from an
/// already-resolved audio file path (e.g. a macOS-auto-mounted CDDA AIFF).
///
/// Same codec / quality switch as [`build_ffmpeg_command`]. Drops `-f` /
/// `-map`: ffmpeg infers the format from the file extension, and AIFF
/// inputs expose one stream so no map selection is needed. `-vn` strips
/// any incidental video/picture stream (a no-op for CDDA AIFFs, kept
/// here so this path stays correct if a caller ever passes a tagged
/// M4A or similar).
pub fn build_ffmpeg_command_from_file(
    input: &Path,
    fidelity: RipFidelity,
    output: &Path,
) -> (PathBuf, Vec<String>) {
    let mut args: Vec<String> = vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "warning".into(),
        "-y".into(),
        // Progress on stdout. See `build_ffmpeg_command` for the rationale.
        "-progress".into(),
        "pipe:1".into(),
        "-i".into(),
        input.to_string_lossy().into_owned(),
        "-vn".into(),
    ];

    extend_codec_args(&mut args, fidelity);

    args.push(output.to_string_lossy().into_owned());
    (PathBuf::from("ffmpeg"), args)
}

/// On macOS, audio CDs auto-mount under `/Volumes/<title>/` exposing each
/// track as `<N> <title>.aiff` alongside a `.TOC.plist` marker file.
/// Reading these directly with `ffmpeg -i <aiff>` works with stock
/// Homebrew ffmpeg (which does not ship `--enable-libcdio`).
#[cfg(target_os = "macos")]
pub fn find_macos_cd_aiff(track_number: u8) -> Option<PathBuf> {
    find_aiff_under_volumes_root(Path::new("/Volumes"), track_number)
}

/// Test seam for [`find_macos_cd_aiff`]: scan `volumes_root/*/` for
/// directories containing a `.TOC.plist` marker, then for each such
/// directory return the first `<track_number> ...aiff` file. Exposed
/// at module visibility so unit tests can point it at a tempdir.
///
/// `find_macos_cd_aiff` is the only non-test caller and it's gated to
/// macOS, so on Linux the function looks dead to clippy in non-test
/// builds. Allow it — the unit tests do exercise it on all platforms.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn find_aiff_under_volumes_root(volumes_root: &Path, track_number: u8) -> Option<PathBuf> {
    let entries = std::fs::read_dir(volumes_root).ok()?;
    for entry in entries.flatten() {
        let volume = entry.path();
        if !volume.join(".TOC.plist").exists() {
            continue;
        }
        let Ok(tracks) = std::fs::read_dir(&volume) else {
            continue;
        };
        for t in tracks.flatten() {
            let name = t.file_name();
            let name_str = name.to_string_lossy();
            // CDDA AIFF files use the `.aiff` extension; macOS exposes
            // the audio as AIFF-C inside that container.
            let ext_matches = std::path::Path::new(&*name_str)
                .extension()
                .is_some_and(|x| x.eq_ignore_ascii_case("aiff"));
            if !ext_matches {
                continue;
            }
            // Filename shape: `<N> <title>.aiff` (single space after N).
            // Split on whitespace and parse the first token so "10 …"
            // doesn't get mis-matched as track 1.
            let Some(prefix) = name_str.split_whitespace().next() else {
                continue;
            };
            if prefix.parse::<u8>().ok() == Some(track_number) {
                return Some(t.path());
            }
        }
    }
    None
}

/// Build the ffmpeg command appropriate for the host. macOS prefers the
/// auto-mounted AIFF (stock Homebrew ffmpeg ships without
/// `--enable-libcdio`); Linux uses the libcdio demuxer addressed by the
/// device node. Returns `None` on macOS when no audio CD is mounted —
/// the caller surfaces a clear "insert disc" error rather than handing
/// off a guaranteed-to-fail libcdio invocation.
#[cfg(target_os = "macos")]
pub fn build_command_for_track(
    drive_device: &Path,
    track_number: u8,
    fidelity: RipFidelity,
    output: &Path,
) -> Result<(PathBuf, Vec<String>), RipError> {
    // Linux-only param on this branch — kept on the signature for
    // cross-platform parity at the call site.
    let _ = drive_device;
    match find_macos_cd_aiff(track_number) {
        Some(aiff) => Ok(build_ffmpeg_command_from_file(&aiff, fidelity, output)),
        None => Err(RipError::CdNotMounted(track_number)),
    }
}

#[cfg(not(target_os = "macos"))]
pub fn build_command_for_track(
    drive_device: &Path,
    track_number: u8,
    fidelity: RipFidelity,
    output: &Path,
) -> Result<(PathBuf, Vec<String>), RipError> {
    Ok(build_ffmpeg_command(
        drive_device,
        track_number,
        fidelity,
        output,
    ))
}

/// Validate that `toc` actually contains the requested track number.
///
/// Returns the track entry on success; the caller uses it for length-based
/// progress estimation in Phase 3.
pub fn require_track(
    toc: &DiscToc,
    track_number: u8,
) -> Result<&super::discid::TocTrack, RipError> {
    toc.tracks
        .iter()
        .find(|t| t.number == track_number)
        .ok_or(RipError::UnknownTrack(track_number))
}

/// Rip a single track, blocking until ffmpeg exits.
///
/// `stderr` is captured into the `RipError::FfmpegFailed` variant on
/// non-zero exit so callers see what went wrong instead of an opaque exit
/// code. Progress (`out_time_us` from ffmpeg's `-progress pipe:1`) is
/// streamed to the caller via a no-op callback in this convenience wrapper;
/// the worker uses [`rip_track_cancellable`] for live per-track progress.
pub fn rip_track(
    drive_device: &Path,
    toc: &DiscToc,
    track_number: u8,
    fidelity: RipFidelity,
    output: &Path,
) -> Result<PathBuf, RipError> {
    rip_track_cancellable(
        drive_device,
        toc,
        track_number,
        fidelity,
        output,
        &|| false,
        &|_| {},
    )
}

/// Like [`rip_track`] but consults `is_cancelled` between IO operations and
/// kills the ffmpeg child if it ever returns `true`. Used by the background
/// worker for `BgCommand::CancelRip` support.
///
/// `on_progress` receives the elapsed audio time in microseconds, parsed
/// from ffmpeg's `-progress pipe:1` `out_time_us` lines. The worker turns
/// these into `RipEvent::Progress` events so the TUI can render
/// `M:SS / M:SS — NN%` against the MB-provided track length.
pub fn rip_track_cancellable(
    drive_device: &Path,
    toc: &DiscToc,
    track_number: u8,
    fidelity: RipFidelity,
    output: &Path,
    is_cancelled: &dyn Fn() -> bool,
    on_progress: &dyn Fn(u64),
) -> Result<PathBuf, RipError> {
    use std::sync::{Arc, Mutex};

    require_track(toc, track_number)?;
    let (program, args) = build_command_for_track(drive_device, track_number, fidelity, output)?;
    let mut child = std::process::Command::new(&program)
        .args(&args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| RipError::Spawn(e.to_string()))?;

    // Drain ffmpeg stdout on a background thread, parsing the key=value
    // `-progress` block to surface elapsed audio time. ffmpeg emits
    // blocks shaped like:
    //   out_time_us=12345678
    //   progress=continue
    //   ...
    //   progress=end
    // We only care about `out_time_us`; everything else (frame, fps,
    // bitrate, total_size, dup_frames, drop_frames) is ignored. The
    // borrowed `on_progress` callback can't be moved into a thread, so
    // we forward elapsed-µs values through an mpsc channel and drain
    // it from the polling loop below — same loop that already polls
    // `is_cancelled`, so no extra wake-up cost.
    let (progress_tx, progress_rx) = std::sync::mpsc::channel::<u64>();
    let stdout_handle = child.stdout.take().map(|stdout| {
        std::thread::spawn(move || {
            use std::io::{BufRead, BufReader};
            let reader = BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                if let Some(rest) = line.strip_prefix("out_time_us=") {
                    if let Ok(us) = rest.trim().parse::<u64>() {
                        let _ = progress_tx.send(us);
                    }
                }
            }
        })
    });

    // Drain ffmpeg stderr on a background thread. The previous
    // implementation only read stderr *after* `try_wait` reported
    // completion, which deadlocks if ffmpeg fills the ~64 KB pipe buffer
    // before exiting (scratched discs produce many recoverable-error
    // warnings). Now we read continuously into a shared `Vec<u8>`.
    let stderr_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let drain_handle = child.stderr.take().map(|mut stderr| {
        let buf = Arc::clone(&stderr_buf);
        std::thread::spawn(move || {
            use std::io::Read;
            let mut chunk = [0u8; 4096];
            loop {
                match stderr.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if let Ok(mut b) = buf.lock() {
                            b.extend_from_slice(&chunk[..n]);
                        }
                    }
                }
            }
        })
    });

    // Poll cancel flag every 100 ms. A track typically takes 30 s-5 min so
    // we're not burning meaningful CPU here. Same poll services the
    // progress channel — ffmpeg emits an `out_time_us` line every ~100 ms,
    // so the user sees smooth updates without a separate timer thread.
    let drain_progress = |rx: &std::sync::mpsc::Receiver<u64>| {
        while let Ok(us) = rx.try_recv() {
            on_progress(us);
        }
    };
    let exit_outcome: Result<Option<i32>, RipError> = loop {
        drain_progress(&progress_rx);
        if is_cancelled() {
            // Cancel-during-completion race: ffmpeg may have already
            // finished in the ~100 ms window before we noticed the
            // cancel flag. Poll one more time before killing — if the
            // child has already exited successfully, accept it instead
            // of pretending the user-cancelled-and-deleted a completed
            // rip.
            if let Ok(Some(status)) = child.try_wait() {
                if status.success() {
                    break Ok(status.code());
                }
                break Err(RipError::FfmpegFailed {
                    exit_code: status.code(),
                    stderr: String::new(),
                });
            }
            // Truly cancelled mid-rip — best-effort kill.
            let _ = child.kill();
            let _ = child.wait();
            break Err(RipError::Cancelled);
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                if status.success() {
                    break Ok(status.code());
                }
                break Err(RipError::FfmpegFailed {
                    exit_code: status.code(),
                    stderr: String::new(), // populated below from drained buffer
                });
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(100)),
            Err(e) => break Err(RipError::Spawn(e.to_string())),
        }
    };

    // Wait for the drain threads to finish so the buffers are complete
    // and the stdout EOF clears any leftover progress lines.
    if let Some(h) = drain_handle {
        let _ = h.join();
    }
    if let Some(h) = stdout_handle {
        let _ = h.join();
    }
    // Drain any final progress lines that landed between the last poll
    // and the stdout EOF.
    drain_progress(&progress_rx);
    let stderr = stderr_buf
        .lock()
        .ok()
        .map(|b| {
            String::from_utf8_lossy(&b)
                .lines()
                .filter(|l| !l.trim().is_empty())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();

    match exit_outcome {
        Ok(_) => Ok(output.to_path_buf()),
        Err(RipError::FfmpegFailed { exit_code, .. }) => {
            Err(RipError::FfmpegFailed { exit_code, stderr })
        }
        Err(other) => Err(other),
    }
}

/// Eject the disc in `drive_device`. Shells out to `drutil` on macOS and
/// `eject` on Linux. Errors surface as a string but the caller is
/// expected to treat ejection as best-effort — a rip that succeeded
/// followed by an eject failure shouldn't be reported as a failed rip.
///
/// **macOS:** `drutil eject` operates on the system default optical drive
/// (no per-device path argument). This is fine in practice — Mac hardware
/// has at most one internal optical drive and external USB drives are
/// universally addressed through the same DriverServices API. If a future
/// user reports a multi-drive Mac (e.g. external + Thunderbolt dock),
/// switch to `diskutil eject <device>` which is `drive_device`-aware.
pub fn eject_drive(drive_device: &Path) -> Result<(), String> {
    let (program, args): (&str, Vec<String>) = if cfg!(target_os = "macos") {
        // See doc comment — `drive_device` intentionally unused here.
        let _ = drive_device;
        ("drutil", vec!["eject".into()])
    } else {
        ("eject", vec![drive_device.to_string_lossy().into_owned()])
    };
    let status = std::process::Command::new(program)
        .args(&args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|e| format!("failed to spawn {program}: {e}"))?;
    if !status.success() {
        return Err(format!("{program} exited with {:?}", status.code()));
    }
    Ok(())
}

#[derive(Debug)]
pub enum RipError {
    /// Requested track number isn't in the disc's TOC.
    UnknownTrack(u8),
    /// `ffmpeg` could not be spawned (missing binary, permission denied).
    Spawn(String),
    /// `ffmpeg` exited non-zero. `stderr` carries the diagnostic output ffmpeg
    /// wrote before exiting; `exit_code` is `None` when terminated by signal.
    FfmpegFailed {
        exit_code: Option<i32>,
        stderr: String,
    },
    /// User cancelled the rip via [`rip_track_cancellable`]'s `is_cancelled`
    /// flag. The output file may exist but is incomplete and should be
    /// deleted by the caller.
    Cancelled,
    /// macOS-only: the audio CD isn't mounted under `/Volumes/<title>/`, so
    /// no AIFF source file could be located for the requested track. We
    /// surface this distinctly rather than passing through to libcdio
    /// because stock Homebrew ffmpeg doesn't ship that demuxer.
    CdNotMounted(u8),
}

impl std::fmt::Display for RipError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RipError::UnknownTrack(n) => write!(f, "track {n} not present in disc TOC"),
            RipError::Spawn(e) => write!(f, "failed to spawn ffmpeg: {e}"),
            RipError::FfmpegFailed {
                exit_code: Some(c),
                stderr,
            } => {
                write!(f, "ffmpeg exited with status {c}: {stderr}")
            }
            RipError::FfmpegFailed {
                exit_code: None,
                stderr,
            } => {
                write!(f, "ffmpeg terminated without status: {stderr}")
            }
            RipError::Cancelled => write!(f, "rip cancelled by user"),
            RipError::CdNotMounted(track) => write!(
                f,
                "audio CD not mounted (looked for track {track} under /Volumes/*/.TOC.plist) — insert the disc and wait for macOS to mount it"
            ),
        }
    }
}

impl std::error::Error for RipError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cd::discid::TocTrack;

    fn small_toc() -> DiscToc {
        DiscToc {
            first_track: 1,
            last_track: 2,
            lead_out_lba: 100_000,
            tracks: vec![
                TocTrack {
                    number: 1,
                    offset_lba: 150,
                },
                TocTrack {
                    number: 2,
                    offset_lba: 50_000,
                },
            ],
        }
    }

    #[test]
    fn build_command_uses_libcdio_input() {
        let (prog, args) = build_ffmpeg_command(
            Path::new("/dev/rdisk4"),
            1,
            RipFidelity::Mp3V0,
            Path::new("/tmp/track1.mp3"),
        );
        assert_eq!(prog.to_str(), Some("ffmpeg"));
        assert!(args.iter().any(|a| a == "libcdio"));
        assert!(args.iter().any(|a| a == "/dev/rdisk4"));
        assert!(args.iter().any(|a| a == "/tmp/track1.mp3"));
    }

    #[test]
    fn build_command_zero_indexes_track_for_map() {
        let (_p, args) = build_ffmpeg_command(
            Path::new("/dev/sr0"),
            3,
            RipFidelity::Mp3Cbr320,
            Path::new("/tmp/t3.mp3"),
        );
        let map_idx = args.iter().position(|a| a == "-map").expect("-map present");
        assert_eq!(args[map_idx + 1], "0:2");
    }

    #[test]
    fn build_command_track_one_maps_to_index_zero() {
        let (_p, args) = build_ffmpeg_command(
            Path::new("/dev/sr0"),
            1,
            RipFidelity::Wav,
            Path::new("/tmp/t1.wav"),
        );
        let map_idx = args.iter().position(|a| a == "-map").unwrap();
        assert_eq!(args[map_idx + 1], "0:0");
    }

    #[test]
    fn build_command_mp3_320_sets_cbr_bitrate() {
        let (_p, args) = build_ffmpeg_command(
            Path::new("/dev/sr0"),
            1,
            RipFidelity::Mp3Cbr320,
            Path::new("/tmp/t.mp3"),
        );
        assert!(args.iter().any(|a| a == "libmp3lame"));
        let b_idx = args.iter().position(|a| a == "-b:a").unwrap();
        assert_eq!(args[b_idx + 1], "320k");
    }

    #[test]
    fn build_command_flac_uses_flac_codec() {
        let (_p, args) = build_ffmpeg_command(
            Path::new("/dev/sr0"),
            1,
            RipFidelity::Flac,
            Path::new("/tmp/t.flac"),
        );
        let c_idx = args.iter().position(|a| a == "-codec:a").unwrap();
        assert_eq!(args[c_idx + 1], "flac");
    }

    #[test]
    fn fidelity_extensions() {
        assert_eq!(RipFidelity::Mp3Cbr320.extension(), "mp3");
        assert_eq!(RipFidelity::Mp3V0.extension(), "mp3");
        assert_eq!(RipFidelity::Mp3V2.extension(), "mp3");
        assert_eq!(RipFidelity::Aac.extension(), "m4a");
        assert_eq!(RipFidelity::Alac.extension(), "m4a");
        assert_eq!(RipFidelity::Flac.extension(), "flac");
        assert_eq!(RipFidelity::Wav.extension(), "wav");
    }

    #[test]
    fn all_fidelities_enumerated() {
        // Order matters for the picker — keep it ascending by storage size so
        // a future test fails loudly if the order is shuffled.
        let labels: Vec<_> = RipFidelity::all().iter().map(|f| f.label()).collect();
        assert_eq!(labels.len(), 7);
        assert!(labels[0].contains("V2"));
        assert!(labels[2].contains("AAC"));
        assert!(labels[4].contains("ALAC"));
        assert!(labels[5].contains("FLAC"));
        assert!(labels[6].contains("WAV"));
    }

    #[test]
    fn build_command_aac_sets_256k_cbr() {
        let (_p, args) = build_ffmpeg_command(
            Path::new("/dev/sr0"),
            1,
            RipFidelity::Aac,
            Path::new("/tmp/t.m4a"),
        );
        let c_idx = args.iter().position(|a| a == "-codec:a").unwrap();
        assert_eq!(args[c_idx + 1], "aac");
        let b_idx = args.iter().position(|a| a == "-b:a").unwrap();
        assert_eq!(args[b_idx + 1], "256k");
    }

    #[test]
    fn tag_summary_is_uniform_across_fidelities() {
        // Today every variant maps to the same `RIP_TAG_SUMMARY` const.
        // If a future container-specific tag drop is added (e.g. an M4A
        // field lofty doesn't round-trip), this test fails loudly so the
        // change isn't silent.
        let summaries: Vec<&str> = RipFidelity::all().iter().map(|f| f.tag_summary()).collect();
        let first = summaries[0];
        assert!(!first.is_empty(), "tag_summary must not be empty");
        for s in &summaries {
            assert_eq!(*s, first, "all fidelities currently share the same tag set");
        }
        // Sanity: the const should mention at least the headliner tags so
        // a future shortening doesn't silently strip user-visible context.
        assert!(first.contains("MBIDs"));
        assert!(first.contains("ISRC"));
        assert!(first.contains("AcoustID"));
    }

    #[test]
    fn build_command_sets_explicit_output_format_for_every_fidelity() {
        // ffmpeg's `-f <muxer>` is essential because rip-pipeline temp
        // filenames end in `.part`, defeating extension-based muxer
        // autodetection. Each fidelity must emit a matching `-f` flag.
        for (fidelity, expected_format) in [
            (RipFidelity::Mp3Cbr320, "mp3"),
            (RipFidelity::Mp3V0, "mp3"),
            (RipFidelity::Mp3V2, "mp3"),
            (RipFidelity::Aac, "ipod"),
            (RipFidelity::Alac, "ipod"),
            (RipFidelity::Flac, "flac"),
            (RipFidelity::Wav, "wav"),
        ] {
            // Verify on the file-input path (macOS) — the temp-extension
            // problem only bit there in the original failure mode.
            let (_p, args) = build_ffmpeg_command_from_file(
                Path::new("/tmp/in.aiff"),
                fidelity,
                Path::new("/tmp/out.m4a.part"),
            );
            // There can be more than one `-f` in the libcdio variant
            // (`-f libcdio` for input) but for the file path the only
            // `-f` should be the output muxer.
            let f_positions: Vec<_> = args
                .iter()
                .enumerate()
                .filter(|(_, a)| *a == "-f")
                .map(|(i, _)| i)
                .collect();
            assert_eq!(
                f_positions.len(),
                1,
                "file-input path should emit exactly one `-f` (the output muxer); fidelity {fidelity:?}"
            );
            assert_eq!(
                args[f_positions[0] + 1],
                expected_format,
                "fidelity {fidelity:?} should use -f {expected_format}"
            );

            // libcdio path should also carry the output muxer (the input
            // -f libcdio is still there too).
            let (_p, libcdio_args) = build_ffmpeg_command(
                Path::new("/dev/sr0"),
                1,
                fidelity,
                Path::new("/tmp/out.part"),
            );
            let output_f_count = libcdio_args
                .iter()
                .filter(|a| **a == expected_format)
                .count();
            assert!(
                output_f_count >= 1,
                "libcdio path should also emit -f {expected_format} for fidelity {fidelity:?}"
            );
        }
    }

    #[test]
    fn build_command_alac_uses_alac_codec_and_no_bitrate() {
        let (_p, args) = build_ffmpeg_command(
            Path::new("/dev/sr0"),
            1,
            RipFidelity::Alac,
            Path::new("/tmp/t.m4a"),
        );
        let c_idx = args.iter().position(|a| a == "-codec:a").unwrap();
        assert_eq!(args[c_idx + 1], "alac");
        // Lossless — no bitrate flag should appear.
        assert!(
            !args.iter().any(|a| a == "-b:a"),
            "lossless ALAC must not carry a target bitrate"
        );
    }

    #[test]
    fn require_track_finds_existing() {
        let toc = small_toc();
        let t = require_track(&toc, 2).unwrap();
        assert_eq!(t.offset_lba, 50_000);
    }

    /// The cancel-flag plumbing isn't testable in pure unit-test
    /// scope (it requires a live ffmpeg child to kill), so the
    /// real-hardware verification lives in the manual rip workflow.
    /// What we *can* unit-test is the error display surface — useful
    /// because the worker pattern-matches on the Display output to
    /// distinguish cancel from other failure modes in earlier revs.
    #[test]
    fn rip_error_cancelled_display_uses_lowercase_cancelled() {
        let err = RipError::Cancelled;
        let msg = format!("{err}");
        assert!(
            msg.contains("cancelled"),
            "expected 'cancelled' in Display output, got {msg:?}"
        );
        // Make sure the variant doesn't pretend it has an exit code or
        // stderr content (it has neither — those belong to FfmpegFailed).
        assert!(!msg.contains("status"));
        assert!(!msg.contains("ffmpeg"));
    }

    #[test]
    fn build_from_file_uses_simple_input_no_libcdio() {
        let (prog, args) = build_ffmpeg_command_from_file(
            Path::new("/Volumes/Ray Of Light/1 Drowned World.aiff"),
            RipFidelity::Flac,
            Path::new("/tmp/01.flac"),
        );
        assert_eq!(prog.to_str(), Some("ffmpeg"));
        // The libcdio demuxer is NOT requested on this path.
        assert!(
            !args.iter().any(|a| a == "libcdio"),
            "file-input path must not request -f libcdio"
        );
        // No -map (single-stream AIFF) and no track-position index.
        assert!(
            !args.iter().any(|a| a == "-map"),
            "file-input path must not pass -map"
        );
        // -vn strips any incidental picture stream.
        assert!(args.iter().any(|a| a == "-vn"));
        // -i points at the AIFF.
        let i_idx = args.iter().position(|a| a == "-i").unwrap();
        assert_eq!(
            args[i_idx + 1],
            "/Volumes/Ray Of Light/1 Drowned World.aiff"
        );
    }

    #[test]
    fn build_from_file_carries_codec_args_across_fidelities() {
        // Same codec branches as build_ffmpeg_command — guard against
        // the two paths drifting if a future codec change touches only
        // one of them.
        for (fidelity, expected_codec) in [
            (RipFidelity::Mp3Cbr320, "libmp3lame"),
            (RipFidelity::Aac, "aac"),
            (RipFidelity::Alac, "alac"),
            (RipFidelity::Flac, "flac"),
            (RipFidelity::Wav, "pcm_s16le"),
        ] {
            let (_p, args) = build_ffmpeg_command_from_file(
                Path::new("/tmp/in.aiff"),
                fidelity,
                Path::new("/tmp/out"),
            );
            let c_idx = args.iter().position(|a| a == "-codec:a").unwrap();
            assert_eq!(
                args[c_idx + 1],
                expected_codec,
                "fidelity {fidelity:?} should use {expected_codec}"
            );
        }
    }

    #[test]
    fn find_aiff_under_volumes_root_picks_matching_track_number() {
        // Synthetic /Volumes/-like layout: one mounted "CD" with a
        // .TOC.plist marker and a handful of <N> ...aiff files.
        let root = std::env::temp_dir().join("zytunes-rip-aiff-finder");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let cd = root.join("Ray Of Light");
        std::fs::create_dir_all(&cd).unwrap();
        std::fs::write(cd.join(".TOC.plist"), b"<plist/>").unwrap();
        std::fs::write(cd.join("1 Drowned World.aiff"), b"data").unwrap();
        std::fs::write(cd.join("10 The Power Of Goodbye.aiff"), b"data").unwrap();
        std::fs::write(cd.join("13 Mer Girl.aiff"), b"data").unwrap();
        // Distractor: a regular folder with the same shape but no .TOC.plist
        // must not be picked up.
        let distractor = root.join("Random Folder");
        std::fs::create_dir_all(&distractor).unwrap();
        std::fs::write(distractor.join("1 Decoy.aiff"), b"data").unwrap();

        // Track 1 matches "1 ..." not "10 ..." — the prefix-parse splits
        // on whitespace so the integer comparison is exact.
        let t1 = find_aiff_under_volumes_root(&root, 1).expect("track 1");
        assert_eq!(t1.file_name().unwrap(), "1 Drowned World.aiff");

        let t10 = find_aiff_under_volumes_root(&root, 10).expect("track 10");
        assert_eq!(t10.file_name().unwrap(), "10 The Power Of Goodbye.aiff");

        let t13 = find_aiff_under_volumes_root(&root, 13).expect("track 13");
        assert_eq!(t13.file_name().unwrap(), "13 Mer Girl.aiff");

        // Tracks that aren't on disc → None.
        assert!(find_aiff_under_volumes_root(&root, 99).is_none());
    }

    #[test]
    fn find_aiff_under_volumes_root_returns_none_when_no_cd_volume() {
        // Volumes root exists but no child has a .TOC.plist marker.
        let root = std::env::temp_dir().join("zytunes-rip-aiff-no-cd");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(root.join("Macintosh HD")).unwrap();
        std::fs::write(root.join("Macintosh HD").join("anything"), b"x").unwrap();

        assert!(find_aiff_under_volumes_root(&root, 1).is_none());
    }

    #[test]
    fn cd_not_mounted_display_mentions_track_number() {
        let err = RipError::CdNotMounted(7);
        let msg = format!("{err}");
        assert!(msg.contains("not mounted"), "{msg}");
        assert!(msg.contains("track 7"), "{msg}");
    }

    #[test]
    fn require_track_rejects_missing() {
        let toc = small_toc();
        let err = require_track(&toc, 99).unwrap_err();
        match err {
            RipError::UnknownTrack(n) => assert_eq!(n, 99),
            _ => panic!("wrong error variant"),
        }
    }
}
