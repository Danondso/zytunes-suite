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
/// FLAC and WAV land in the library at full source quality. On push to a
/// device they're transcoded to the highest fidelity that device supports
/// (ALAC for iPod Classic via the FLAC→ALAC ffmpeg branch; LAME VBR
/// NearBest (~V0) for Zune since the Zune firmware has no lossless
/// container) — see `DeviceCapabilities::lossless_target`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RipFidelity {
    /// MP3 CBR 320 kbps.
    Mp3Cbr320,
    /// MP3 VBR ~245 kbps (LAME `-V 0` equivalent).
    Mp3V0,
    /// MP3 VBR ~190 kbps (LAME `-V 2` equivalent).
    Mp3V2,
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
            RipFidelity::Mp3Cbr320,
            RipFidelity::Flac,
            RipFidelity::Wav,
        ]
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

/// Build the ffmpeg command line for ripping a single track from a CD.
///
/// Returns the program name and argv tail so callers can spawn it or inspect
/// it for tests. `drive_device` should be a node ffmpeg can open via the
/// `libcdio` input — on macOS that's typically `/dev/rdiskN`, on Linux
/// `/dev/srN`.
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

    match fidelity {
        RipFidelity::Mp3Cbr320 => {
            args.extend([
                "-codec:a".into(),
                "libmp3lame".into(),
                "-b:a".into(),
                "320k".into(),
            ]);
        }
        RipFidelity::Mp3V0 => {
            args.extend([
                "-codec:a".into(),
                "libmp3lame".into(),
                "-q:a".into(),
                "0".into(),
            ]);
        }
        RipFidelity::Mp3V2 => {
            args.extend([
                "-codec:a".into(),
                "libmp3lame".into(),
                "-q:a".into(),
                "2".into(),
            ]);
        }
        RipFidelity::Flac => {
            args.extend(["-codec:a".into(), "flac".into()]);
        }
        RipFidelity::Wav => {
            args.extend(["-codec:a".into(), "pcm_s16le".into()]);
        }
    }

    args.push(output.to_string_lossy().into_owned());
    (PathBuf::from("ffmpeg"), args)
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
/// code. Per-byte progress streaming is not provided here — Phase 3 emits
/// progress at per-track granularity from the worker (a 4-minute rip
/// produces one "started" event and one "done" event), which is sufficient
/// for the import overlay's status line. Within-track progress would
/// require an ffmpeg `-progress pipe:2` parser; not worth the complexity
/// for the user-perceived improvement (a single track rips in 30s-5min).
pub fn rip_track(
    drive_device: &Path,
    toc: &DiscToc,
    track_number: u8,
    fidelity: RipFidelity,
    output: &Path,
) -> Result<PathBuf, RipError> {
    rip_track_cancellable(drive_device, toc, track_number, fidelity, output, &|| false)
}

/// Like [`rip_track`] but consults `is_cancelled` between IO operations and
/// kills the ffmpeg child if it ever returns `true`. Used by the background
/// worker for `BgCommand::CancelRip` support.
pub fn rip_track_cancellable(
    drive_device: &Path,
    toc: &DiscToc,
    track_number: u8,
    fidelity: RipFidelity,
    output: &Path,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<PathBuf, RipError> {
    use std::sync::{Arc, Mutex};

    require_track(toc, track_number)?;
    let (program, args) = build_ffmpeg_command(drive_device, track_number, fidelity, output);
    let mut child = std::process::Command::new(&program)
        .args(&args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| RipError::Spawn(e.to_string()))?;

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
    // we're not burning meaningful CPU here.
    let exit_outcome: Result<Option<i32>, RipError> = loop {
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

    // Wait for the drain thread to finish so the buffer is complete.
    if let Some(h) = drain_handle {
        let _ = h.join();
    }
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
        assert_eq!(RipFidelity::Flac.extension(), "flac");
        assert_eq!(RipFidelity::Wav.extension(), "wav");
    }

    #[test]
    fn all_fidelities_enumerated() {
        // Order matters for the picker — keep it ascending by storage size so
        // a future test fails loudly if the order is shuffled.
        let labels: Vec<_> = RipFidelity::all().iter().map(|f| f.label()).collect();
        assert_eq!(labels.len(), 5);
        assert!(labels[0].contains("V2"));
        assert!(labels[3].contains("FLAC"));
        assert!(labels[4].contains("WAV"));
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
    fn require_track_rejects_missing() {
        let toc = small_toc();
        let err = require_track(&toc, 99).unwrap_err();
        match err {
            RipError::UnknownTrack(n) => assert_eq!(n, 99),
            _ => panic!("wrong error variant"),
        }
    }
}
