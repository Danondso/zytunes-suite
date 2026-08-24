use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use rodio::{Decoder, DeviceSinkBuilder, Player};
use zytunes::stems::{StemGains, StemSet};

#[path = "audio/stem_mix.rs"]
pub mod stem_mix;
use stem_mix::StemMixerSource;

/// Formats that rodio + symphonia can decode directly (with expanded codec features).
/// WMA is the only common format not supported by symphonia.
const RODIO_NATIVE: &[&str] = &[
    "mp3", "wav", "flac", "ogg", "m4a", "aac", "aiff", "alac", "opus",
];

pub enum AudioCommand {
    Play {
        path: String,
    },
    /// Gaplessly replace the playing source at its current position —
    /// the stem-mode entry/exit transition. Unlike Play+Scrub, the
    /// incoming source is built and pre-seeked while the old one keeps
    /// playing, then cut over in one motion with a short fade-in, so the
    /// listener hears a blend rather than a half-second hole. Pause
    /// state is preserved across the swap.
    SwapSource {
        target: SwapTarget,
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

/// What [`AudioCommand::SwapSource`] swaps to. `Stems` carries the shared
/// gains the app keeps a clone of — toggles are lock-free stores, no
/// further commands needed for mutes/unmutes.
pub enum SwapTarget {
    File {
        path: String,
    },
    Stems {
        stems: Box<StemSet>,
        gains: StemGains,
    },
}

pub enum AudioEvent {
    TrackEnded,
    PlaybackError(String),
    /// A [`AudioCommand::SwapSource`] build failed. Unlike
    /// `PlaybackError`, playback is NOT lost — the old source was left
    /// playing (the swap builds the incoming source before touching the
    /// outgoing one). `to_stems` tells the app which transition to
    /// unwind: a failed stem entry leaves the file playing while the app
    /// prematurely shows Active, so stem state resets; a failed exit
    /// leaves the stems audible and the app already Off — nothing to
    /// unwind beyond telling the user.
    SwapFailed {
        error: String,
        to_stems: bool,
    },
    /// A [`AudioCommand::Scrub`] rebuild failed (source file vanished or
    /// went undecodable mid-play). The seek didn't happen; the previous
    /// source keeps playing at its old position.
    SeekFailed(String),
    Position {
        elapsed_ms: u64,
    },
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

/// What the player is currently rendering — needed by Scrub, which
/// rebuilds the source from scratch to seek.
enum NowSource {
    File(String),
    Stems(Box<StemSet>, StemGains),
}

/// Open and decode `path`, containing the panics rodio/symphonia can
/// throw on malformed files (seek errors) so they surface as errors
/// instead of killing the audio thread. Shared by the single-file, stem,
/// and scrub paths.
fn build_decoder(path: &str) -> Result<Decoder<BufReader<File>>, String> {
    let file = File::open(path).map_err(|e| format!("Open: {}", e))?;
    let reader = BufReader::new(file);
    let hint = format_hint(path);
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut builder = Decoder::builder().with_data(reader);
        if let Some(ref h) = hint {
            builder = builder.with_hint(h);
        }
        builder.build()
    })) {
        Ok(Ok(s)) => Ok(s),
        Ok(Err(e)) => Err(format!("Decode: {}", e)),
        Err(_) => Err("Decoder crashed — unsupported file".into()),
    }
}

/// Consume `dur` worth of samples from `src` right now, on the calling
/// thread — the fallback when [`seek_source`]'s `try_seek` isn't
/// available. Channel count is re-read per frame so span changes stay
/// approximately correct; sources that end mid-skip are returned
/// exhausted (the mixer pads, files just end).
fn eager_skip<S: rodio::Source>(mut src: S, dur: Duration) -> S {
    let frames = (dur.as_secs_f64() * src.sample_rate().get() as f64) as u64;
    for _ in 0..frames {
        for _ in 0..src.channels().get() {
            if src.next().is_none() {
                return src;
            }
        }
    }
    src
}

/// Position a fresh source at `pos`, on the calling thread. `try_seek`
/// first — rodio's symphonia decoders default to `SeekMode::Accurate`
/// (keyframe seek + refine), so this is near-constant time instead of
/// decoding the whole prefix and freezing the audio-command loop for
/// seconds on late-track swaps. Sources that can't seek (or whose
/// container lacks a time base) fall back to [`eager_skip`]. Only safe
/// on a source still at position zero: the fallback skips relative to
/// wherever the source currently sits.
fn seek_source<S: rodio::Source>(mut src: S, pos: Duration) -> S {
    if pos.is_zero() {
        return src;
    }
    match src.try_seek(pos) {
        Ok(()) => src,
        Err(_) => eager_skip(src, pos),
    }
}

/// Open every stem file in the set's layout and build the mixing source
/// over them, each decoder pre-positioned at `seek`. Seeking happens
/// per-decoder BEFORE the mixer wraps them: each decoder is
/// independently at zero, so a per-decoder `eager_skip` fallback stays
/// sample-consistent, whereas a mixer-level seek that failed halfway
/// would leave the stems at different positions with no way back.
fn build_stem_mixer(
    stems: &StemSet,
    gains: &StemGains,
    seek: Duration,
) -> Result<StemMixerSource<Decoder<BufReader<File>>>, String> {
    let mut decoders = Vec::with_capacity(stems.paths.len());
    for p in &stems.paths {
        decoders.push(seek_source(build_decoder(&p.to_string_lossy())?, seek));
    }
    StemMixerSource::new(decoders, gains.clone())
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
        let mut current: Option<NowSource> = None;

        loop {
            match cmd_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(AudioCommand::Play { path }) => {
                    // Stop and drop any existing player
                    if let Some(old) = player.take() {
                        old.stop();
                    }

                    // Transcode if needed (WMA and other non-symphonia formats)
                    let play_path = match resolve_playback_path(&path) {
                        Ok(p) => p,
                        Err(e) => {
                            let _ = event_tx.send(AudioEvent::PlaybackError(e));
                            continue;
                        }
                    };

                    let source = match build_decoder(&play_path) {
                        Ok(s) => s,
                        Err(e) => {
                            let _ = event_tx.send(AudioEvent::PlaybackError(e));
                            continue;
                        }
                    };

                    let new_player = Player::connect_new(device_sink.mixer());
                    new_player.append(source);
                    play_start = Instant::now();
                    paused_elapsed = Duration::ZERO;
                    playing = true;
                    current = Some(NowSource::File(play_path));
                    player = Some(new_player);
                }
                Ok(AudioCommand::SwapSource { target }) => {
                    use rodio::Source;
                    // Position from this thread's own clock at the moment
                    // the swap arrives — fresher than any app-side
                    // elapsed snapshot could be.
                    let base = if playing {
                        paused_elapsed + play_start.elapsed()
                    } else {
                        paused_elapsed
                    };
                    let build_start = Instant::now();
                    let fade = Duration::from_millis(15);
                    let to_stems = matches!(target, SwapTarget::Stems { .. });

                    // Build and pre-seek the incoming source while the old
                    // one keeps playing — whatever positioning costs, it
                    // happens outside the audible seam, and the old player
                    // isn't touched until the build has succeeded.
                    let built: Result<(NowSource, _), String> = match target {
                        SwapTarget::File { path } => resolve_playback_path(&path)
                            .and_then(|p| build_decoder(&p).map(|s| (p, s)))
                            .map(|(p, s)| {
                                (
                                    NowSource::File(p),
                                    Box::new(seek_source(s, base)) as Box<dyn rodio::Source + Send>,
                                )
                            }),
                        SwapTarget::Stems { stems, gains } => {
                            build_stem_mixer(&stems, &gains, base).map(|m| {
                                (
                                    NowSource::Stems(stems, gains),
                                    Box::new(m) as Box<dyn rodio::Source + Send>,
                                )
                            })
                        }
                    };
                    match built {
                        Ok((src, mut positioned)) => {
                            // Catch up to wherever the still-playing old
                            // source has advanced during the build. Loop
                            // because the catch-up skip itself takes time
                            // (a lot of it on the eager fallback); each
                            // pass closes the remaining drift until it's
                            // below the fade length, so the cut-over
                            // lands where the music actually is.
                            let mut pos = base;
                            if playing {
                                for _ in 0..3 {
                                    let drift = (base + build_start.elapsed()).saturating_sub(pos);
                                    if drift <= fade {
                                        break;
                                    }
                                    positioned = Box::new(eager_skip(positioned, drift));
                                    pos += drift;
                                }
                            }
                            let new_player = Player::connect_new(device_sink.mixer());
                            if !playing {
                                // Pause before append so a paused swap
                                // can't leak a few callback-buffers of
                                // sound.
                                new_player.pause();
                            }
                            new_player.append(positioned.fade_in(fade));
                            // Cut over: the new source starts on the next
                            // output callback; stopping the old
                            // immediately after leaves a near-zero seam
                            // that the fade-in masks.
                            if let Some(old) = player.take() {
                                old.stop();
                            }
                            paused_elapsed = pos;
                            play_start = Instant::now();
                            current = Some(src);
                            player = Some(new_player);
                        }
                        Err(e) => {
                            // The old source was never touched — keep it
                            // playing and let the app unwind only the
                            // transition state. Killing healthy playback
                            // over a failed swap (e.g. a stem cache entry
                            // pruned between check and build) punished
                            // the listener for our problem.
                            let _ = event_tx.send(AudioEvent::SwapFailed { error: e, to_stems });
                        }
                    }
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
                    let elapsed_now = if playing {
                        paused_elapsed + play_start.elapsed()
                    } else {
                        paused_elapsed
                    };
                    let current_ms = elapsed_now.as_millis() as i64;
                    let new_ms = (current_ms + delta_ms).max(0) as u64;
                    let new_pos = Duration::from_millis(new_ms);

                    // Re-open the source pre-positioned at the new spot.
                    // Stem playback rebuilds the mixer over the same gains
                    // Arc, so toggle state survives the seek. Build BEFORE
                    // stopping the old player: a failed rebuild (file
                    // vanished, cache entry pruned) keeps the music
                    // playing at its old position instead of leaving a
                    // phantom Playing state with no player — the old
                    // silent-swallow here wedged the UI with a forever-
                    // advancing clock and no possible TrackEnded.
                    if let Some(ref src) = current {
                        let new_player = Player::connect_new(device_sink.mixer());
                        if !playing {
                            new_player.pause();
                        }
                        let appended = match src {
                            NowSource::File(path) => build_decoder(path)
                                .map(|s| new_player.append(seek_source(s, new_pos))),
                            NowSource::Stems(stems, gains) => {
                                build_stem_mixer(stems, gains, new_pos)
                                    .map(|m| new_player.append(m))
                            }
                        };
                        match appended {
                            Ok(()) => {
                                if let Some(old) = player.take() {
                                    old.stop();
                                }
                                paused_elapsed = new_pos;
                                play_start = Instant::now();
                                player = Some(new_player);
                            }
                            Err(e) => {
                                new_player.stop();
                                let _ = event_tx.send(AudioEvent::SeekFailed(e));
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

    /// Minimal mono 44.1 kHz / 16-bit RIFF WAV of `frames` zero samples —
    /// enough for symphonia to decode; keeps the fixture local since the
    /// lib's test_audio helpers aren't visible from the binary's tests.
    fn write_test_wav(path: &Path, frames: u32) {
        use std::io::Write;
        let data_size = frames * 2;
        let mut f = File::create(path).unwrap();
        f.write_all(b"RIFF").unwrap();
        f.write_all(&(36 + data_size).to_le_bytes()).unwrap();
        f.write_all(b"WAVEfmt ").unwrap();
        f.write_all(&16u32.to_le_bytes()).unwrap();
        f.write_all(&1u16.to_le_bytes()).unwrap(); // PCM
        f.write_all(&1u16.to_le_bytes()).unwrap(); // mono
        f.write_all(&44_100u32.to_le_bytes()).unwrap();
        f.write_all(&(44_100u32 * 2).to_le_bytes()).unwrap();
        f.write_all(&2u16.to_le_bytes()).unwrap();
        f.write_all(&16u16.to_le_bytes()).unwrap();
        f.write_all(b"data").unwrap();
        f.write_all(&data_size.to_le_bytes()).unwrap();
        f.write_all(&vec![0u8; data_size as usize]).unwrap();
    }

    #[test]
    fn eager_skip_consumes_immediately_and_survives_overrun() {
        use rodio::buffer::SamplesBuffer;
        use std::num::NonZero;
        // 2-channel source at 100 Hz with recognisable sample values.
        let data: Vec<f32> = (0..200).map(|i| i as f32).collect();
        let src = SamplesBuffer::new(NonZero::new(2).unwrap(), NonZero::new(100).unwrap(), data);

        // 0.25 s = 25 frames = 50 interleaved samples skipped, eagerly.
        let mut out = eager_skip(src, Duration::from_millis(250));
        assert_eq!(out.next(), Some(50.0));

        // Skipping far past the end exhausts cleanly instead of panicking.
        let rest = eager_skip(out, Duration::from_secs(60));
        assert_eq!(rest.count(), 0);
    }

    #[test]
    fn build_stem_mixer_opens_six_stems_and_reports_missing_files() {
        let dir = std::env::temp_dir().join(format!(
            "zytunes-audio-stems-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Cache entries are FLAC in production, but the mixer builder is
        // extension-driven via format_hint, so WAV fixtures exercise the
        // same open/decode path without an encoder dependency.
        let stems = StemSet::from_layout(&dir, "wav", zytunes::stems::SIX_STEM_LAYOUT);
        for p in &stems.paths {
            write_test_wav(p, 64);
        }
        let gains = zytunes::stems::new_stem_gains(&[true; 6]);
        let mixer = build_stem_mixer(&stems, &gains, Duration::ZERO).expect("all six stems decode");
        use rodio::Source;
        assert_eq!(mixer.channels().get(), 1);
        assert_eq!(mixer.sample_rate().get(), 44_100);

        // Pre-seeked build: half the 64 frames consumed before mixing, so
        // the mix yields exactly the remaining half. Exercises the
        // seek-before-wrap path the swap and stem scrub rely on.
        let seek = Duration::from_secs_f64(32.0 / 44_100.0);
        let mixer = build_stem_mixer(&stems, &gains, seek).expect("seeked build");
        assert_eq!(mixer.count(), 32, "half the frames remain after seek");

        std::fs::remove_file(&stems.paths[2]).unwrap();
        let err = build_stem_mixer(&stems, &gains, Duration::ZERO)
            .err()
            .expect("missing stem must fail the build");
        assert!(err.contains("Open"), "{err}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn seek_source_positions_decoders_without_full_decode_and_falls_back() {
        // A real file decoder: try_seek is supported (accurate mode), so
        // the source lands at the requested position.
        let dir = std::env::temp_dir().join(format!(
            "zytunes-audio-seek-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("t.wav");
        write_test_wav(&wav, 4_410); // 100 ms of mono 44.1 kHz
        let decoder = build_decoder(&wav.to_string_lossy()).unwrap();
        let sought = seek_source(decoder, Duration::from_millis(50));
        let remaining = sought.count();
        assert!(
            (2_150..=2_260).contains(&remaining),
            "~50 ms should remain after the seek, got {remaining} samples"
        );

        // A source whose try_seek is unsupported falls back to the eager
        // skip and still lands at the same place.
        struct NoSeek<S: rodio::Source>(S);
        impl<S: rodio::Source> Iterator for NoSeek<S> {
            type Item = rodio::Sample;
            fn next(&mut self) -> Option<rodio::Sample> {
                self.0.next()
            }
        }
        impl<S: rodio::Source> rodio::Source for NoSeek<S> {
            fn current_span_len(&self) -> Option<usize> {
                self.0.current_span_len()
            }
            fn channels(&self) -> rodio::ChannelCount {
                self.0.channels()
            }
            fn sample_rate(&self) -> rodio::SampleRate {
                self.0.sample_rate()
            }
            fn total_duration(&self) -> Option<Duration> {
                self.0.total_duration()
            }
            // No try_seek override: the default returns NotSupported.
        }
        use rodio::buffer::SamplesBuffer;
        use std::num::NonZero;
        let data: Vec<f32> = (0..100).map(|i| i as f32).collect();
        let buf = SamplesBuffer::new(NonZero::new(1).unwrap(), NonZero::new(100).unwrap(), data);
        let mut sought = seek_source(NoSeek(buf), Duration::from_millis(250));
        assert_eq!(sought.next(), Some(25.0), "eager fallback hit the mark");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
