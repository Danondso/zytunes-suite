use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use rodio::{Decoder, DeviceSinkBuilder, Player};

/// Formats that rodio + symphonia can decode directly (with expanded codec features).
/// WMA is the only common format not supported by symphonia.
const RODIO_NATIVE: &[&str] = &[
    "mp3", "wav", "flac", "ogg", "m4a", "aac", "aiff", "alac", "opus",
];

pub enum AudioCommand {
    Play {
        path: String,
    },
    Pause,
    Resume,
    Stop,
    QueryPosition,
    /// Seek forward/backward by this many milliseconds (negative = rewind).
    Scrub {
        delta_ms: i64,
    },
}

pub enum AudioEvent {
    TrackEnded,
    PlaybackError(String),
    Position { elapsed_ms: u64 },
}

/// Returns true if the file extension requires transcoding before playback.
fn needs_transcode_for_playback(path: &str) -> bool {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    !RODIO_NATIVE.contains(&ext.as_str())
}

/// Extract a format hint for rodio/symphonia from a file path.
/// ALAC files use the ISO MP4 container, so we hint "m4a" for the container
/// format rather than "alac" (which symphonia doesn't recognize as a container).
fn format_hint(path: &str) -> Option<String> {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())?
        .to_lowercase();
    match ext.as_str() {
        "alac" => Some("m4a".to_string()),
        other => Some(other.to_string()),
    }
}

/// Transcode an unsupported file (e.g. WMA) to WAV via ffmpeg for playback.
/// Returns the path to the transcoded temp file.
fn transcode_for_playback(input: &str) -> Result<String, String> {
    let temp_dir = std::env::temp_dir().join("zytunes-playback");
    std::fs::create_dir_all(&temp_dir).map_err(|e| format!("Temp dir: {}", e))?;

    let stem = Path::new(input)
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy();
    let output = temp_dir.join(format!("{stem}.wav"));

    // Skip if already transcoded
    if output.exists() {
        return Ok(output.to_string_lossy().into_owned());
    }

    let result = Command::new("ffmpeg")
        .args([
            "-y",
            "-i",
            input,
            "-vn",
            "-acodec",
            "pcm_s16le",
            "-ar",
            "44100",
        ])
        .arg(&output)
        .output()
        .map_err(|e| format!("ffmpeg: {}", e))?;

    if !result.status.success() {
        let stderr = String::from_utf8_lossy(&result.stderr);
        return Err(format!(
            "ffmpeg failed: {}",
            stderr.lines().last().unwrap_or("unknown")
        ));
    }

    Ok(output.to_string_lossy().into_owned())
}

/// Resolve the playback path — transcode if needed, or return the original.
fn resolve_playback_path(path: &str) -> Result<String, String> {
    if needs_transcode_for_playback(path) {
        transcode_for_playback(path)
    } else {
        Ok(path.to_string())
    }
}

pub fn spawn(event_tx: mpsc::Sender<AudioEvent>) -> mpsc::Sender<AudioCommand> {
    let (cmd_tx, cmd_rx) = mpsc::channel::<AudioCommand>();

    thread::spawn(move || {
        let mut device_sink = match DeviceSinkBuilder::open_default_sink() {
            Ok(s) => s,
            Err(e) => {
                let _ = event_tx.send(AudioEvent::PlaybackError(format!("Audio output: {}", e)));
                return;
            }
        };
        // Suppress rodio's drop-time eprintln which would corrupt the TUI.
        device_sink.log_on_drop(false);

        let mut player: Option<Player> = None;
        let mut playing = false;
        let mut play_start = Instant::now();
        let mut paused_elapsed = Duration::ZERO;
        let mut current_path: Option<String> = None;

        loop {
            match cmd_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(AudioCommand::Play { path }) => {
                    // Stop and drop any existing player
                    if let Some(old) = player.take() {
                        old.stop();
                    }

                    // Transcode if needed (ALAC, AIFF, OPUS, etc.)
                    let play_path = match resolve_playback_path(&path) {
                        Ok(p) => p,
                        Err(e) => {
                            let _ = event_tx.send(AudioEvent::PlaybackError(e));
                            continue;
                        }
                    };

                    let new_player = Player::connect_new(device_sink.mixer());

                    let file = match File::open(&play_path) {
                        Ok(f) => f,
                        Err(e) => {
                            let _ =
                                event_tx.send(AudioEvent::PlaybackError(format!("Open: {}", e)));
                            continue;
                        }
                    };

                    // rodio can panic on certain files (seek errors in symphonia)
                    let reader = BufReader::new(file);
                    let hint = format_hint(&play_path);
                    let source =
                        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            let mut builder = Decoder::builder().with_data(reader);
                            if let Some(ref h) = hint {
                                builder = builder.with_hint(h);
                            }
                            builder.build()
                        })) {
                            Ok(Ok(s)) => s,
                            Ok(Err(e)) => {
                                let _ = event_tx
                                    .send(AudioEvent::PlaybackError(format!("Decode: {}", e)));
                                continue;
                            }
                            Err(_) => {
                                let _ = event_tx.send(AudioEvent::PlaybackError(
                                    "Decoder crashed — unsupported file".into(),
                                ));
                                continue;
                            }
                        };

                    new_player.append(source);
                    play_start = Instant::now();
                    paused_elapsed = Duration::ZERO;
                    playing = true;
                    current_path = Some(play_path);
                    player = Some(new_player);
                }
                Ok(AudioCommand::Stop) => {
                    if let Some(p) = player.take() {
                        p.stop();
                    }
                    playing = false;
                    paused_elapsed = Duration::ZERO;
                }
                Ok(AudioCommand::Pause) => {
                    if let Some(ref p) = player {
                        p.pause();
                    }
                    if playing {
                        paused_elapsed += play_start.elapsed();
                    }
                    playing = false;
                }
                Ok(AudioCommand::Resume) => {
                    if let Some(ref p) = player {
                        p.play();
                    }
                    play_start = Instant::now();
                    playing = true;
                }
                Ok(AudioCommand::QueryPosition) => {
                    let elapsed = if playing {
                        paused_elapsed + play_start.elapsed()
                    } else {
                        paused_elapsed
                    };
                    let _ = event_tx.send(AudioEvent::Position {
                        elapsed_ms: elapsed.as_millis() as u64,
                    });
                }
                Ok(AudioCommand::Scrub { delta_ms }) => {
                    // Compute new position
                    let current = if playing {
                        paused_elapsed + play_start.elapsed()
                    } else {
                        paused_elapsed
                    };
                    let current_ms = current.as_millis() as i64;
                    let new_ms = (current_ms + delta_ms).max(0) as u64;
                    let new_pos = Duration::from_millis(new_ms);

                    // Re-open file and skip to new position
                    if let Some(ref path) = current_path {
                        if let Some(old) = player.take() {
                            old.stop();
                        }
                        let new_player = Player::connect_new(device_sink.mixer());
                        if let Ok(file) = File::open(path) {
                            let reader = BufReader::new(file);
                            if let Ok(Ok(source)) =
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    Decoder::new(reader)
                                }))
                            {
                                use rodio::Source;
                                new_player.append(source.skip_duration(new_pos));
                                if !playing {
                                    new_player.pause();
                                }
                                paused_elapsed = new_pos;
                                play_start = Instant::now();
                                player = Some(new_player);
                            }
                        }
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if playing {
                        if let Some(ref p) = player {
                            if p.empty() {
                                playing = false;
                                paused_elapsed = Duration::ZERO;
                                let _ = event_tx.send(AudioEvent::TrackEnded);
                            }
                        }
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    if let Some(p) = player.take() {
                        p.stop();
                    }
                    return;
                }
            }
        }
    });

    cmd_tx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn needs_transcode_expanded_formats_are_native() {
        // All these should now be playable natively via symphonia
        assert!(!needs_transcode_for_playback("song.mp3"));
        assert!(!needs_transcode_for_playback("song.wav"));
        assert!(!needs_transcode_for_playback("song.flac"));
        assert!(!needs_transcode_for_playback("song.ogg"));
        assert!(!needs_transcode_for_playback("song.m4a"));
        assert!(!needs_transcode_for_playback("song.aac"));
        assert!(!needs_transcode_for_playback("song.aiff"));
        assert!(!needs_transcode_for_playback("song.alac"));
        assert!(!needs_transcode_for_playback("song.opus"));
        // Case insensitive
        assert!(!needs_transcode_for_playback("song.FLAC"));
        assert!(!needs_transcode_for_playback("song.M4A"));
    }

    #[test]
    fn format_hint_maps_alac_to_m4a() {
        assert_eq!(format_hint("song.alac"), Some("m4a".to_string()));
        assert_eq!(format_hint("song.ALAC"), Some("m4a".to_string()));
        assert_eq!(format_hint("song.m4a"), Some("m4a".to_string()));
        assert_eq!(format_hint("song.mp3"), Some("mp3".to_string()));
        assert_eq!(format_hint("song.flac"), Some("flac".to_string()));
        assert_eq!(format_hint("noext"), None);
    }

    #[test]
    fn needs_transcode_unsupported_formats() {
        // WMA and unknown formats still need transcode
        assert!(needs_transcode_for_playback("song.wma"));
        assert!(needs_transcode_for_playback("song.xyz"));
        assert!(needs_transcode_for_playback("noext"));
    }
}
