use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use rodio::{Decoder, OutputStream, Sink};

/// Formats that rodio + symphonia can decode directly.
/// M4A excluded — symphonia panics on some M4A files (ALAC-encoded, seek errors).
/// AAC excluded too — raw .aac files are rare and M4A container is more common.
const RODIO_NATIVE: &[&str] = &["mp3", "wav", "flac", "ogg"];

pub enum AudioCommand {
    Play { path: String },
    Pause,
    Resume,
    Stop,
    QueryPosition,
    /// Seek forward/backward by this many milliseconds (negative = rewind).
    Scrub { delta_ms: i64 },
}

pub enum AudioEvent {
    TrackEnded,
    PlaybackError(String),
    Position { elapsed_ms: u64 },
}

/// Returns true if the file extension requires ffmpeg transcoding before playback.
fn needs_transcode_for_playback(path: &str) -> bool {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    !RODIO_NATIVE.contains(&ext.as_str())
}

/// Transcode an unsupported file to WAV via ffmpeg for playback.
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
        .args(["-y", "-i", input, "-vn", "-acodec", "pcm_s16le", "-ar", "44100"])
        .arg(&output)
        .output()
        .map_err(|e| format!("ffmpeg: {}", e))?;

    if !result.status.success() {
        let stderr = String::from_utf8_lossy(&result.stderr);
        return Err(format!("ffmpeg failed: {}", stderr.lines().last().unwrap_or("unknown")));
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
        let (_stream, stream_handle) = match OutputStream::try_default() {
            Ok(s) => s,
            Err(e) => {
                let _ = event_tx.send(AudioEvent::PlaybackError(format!(
                    "Audio output: {}",
                    e
                )));
                return;
            }
        };

        let mut sink: Option<Sink> = None;
        let mut playing = false;
        let mut play_start = Instant::now();
        let mut paused_elapsed = Duration::ZERO;
        let mut current_path: Option<String> = None;

        loop {
            match cmd_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(AudioCommand::Play { path }) => {
                    // Stop and drop any existing sink
                    if let Some(old) = sink.take() {
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

                    let new_sink = match Sink::try_new(&stream_handle) {
                        Ok(s) => s,
                        Err(e) => {
                            let _ = event_tx.send(AudioEvent::PlaybackError(format!(
                                "Sink: {}",
                                e
                            )));
                            continue;
                        }
                    };

                    let file = match File::open(&play_path) {
                        Ok(f) => f,
                        Err(e) => {
                            let _ = event_tx.send(AudioEvent::PlaybackError(format!(
                                "Open: {}",
                                e
                            )));
                            continue;
                        }
                    };

                    // rodio can panic on certain files (seek errors in symphonia)
                    let reader = BufReader::new(file);
                    let source = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        Decoder::new(reader)
                    })) {
                        Ok(Ok(s)) => s,
                        Ok(Err(e)) => {
                            let _ = event_tx.send(AudioEvent::PlaybackError(format!(
                                "Decode: {}",
                                e
                            )));
                            continue;
                        }
                        Err(_) => {
                            let _ = event_tx.send(AudioEvent::PlaybackError(
                                "Decoder crashed — unsupported file".into(),
                            ));
                            continue;
                        }
                    };

                    new_sink.append(source);
                    play_start = Instant::now();
                    paused_elapsed = Duration::ZERO;
                    playing = true;
                    current_path = Some(play_path);
                    sink = Some(new_sink);
                }
                Ok(AudioCommand::Stop) => {
                    if let Some(s) = sink.take() {
                        s.stop();
                    }
                    playing = false;
                    paused_elapsed = Duration::ZERO;
                }
                Ok(AudioCommand::Pause) => {
                    if let Some(ref s) = sink {
                        s.pause();
                    }
                    if playing {
                        paused_elapsed += play_start.elapsed();
                    }
                    playing = false;
                }
                Ok(AudioCommand::Resume) => {
                    if let Some(ref s) = sink {
                        s.play();
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
                        if let Some(old) = sink.take() {
                            old.stop();
                        }
                        if let Ok(new_sink) = Sink::try_new(&stream_handle) {
                            if let Ok(file) = File::open(path) {
                                let reader = BufReader::new(file);
                                if let Ok(Ok(source)) = std::panic::catch_unwind(
                                    std::panic::AssertUnwindSafe(|| Decoder::new(reader)),
                                ) {
                                    use rodio::Source;
                                    new_sink.append(source.skip_duration(new_pos));
                                    if !playing {
                                        new_sink.pause();
                                    }
                                    paused_elapsed = new_pos;
                                    play_start = Instant::now();
                                    sink = Some(new_sink);
                                }
                            }
                        }
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if playing {
                        if let Some(ref s) = sink {
                            if s.empty() {
                                playing = false;
                                paused_elapsed = Duration::ZERO;
                                let _ = event_tx.send(AudioEvent::TrackEnded);
                            }
                        }
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    if let Some(s) = sink.take() {
                        s.stop();
                    }
                    return;
                }
            }
        }
    });

    cmd_tx
}
