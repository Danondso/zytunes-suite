//! Shared WAV-writing helpers for unit tests across modules.
//!
//! Used by `fingerprint::tests` and `dirlib::tests` so the symphonia decode
//! path can run end-to-end without a real audio fixture in the repo. The
//! lofty-tag-write fixture (`make_test_wav` in `dirlib::tests`) stays local —
//! it's a fixed-size minimal RIFF used only for the lofty round-trip test.

use std::fs;
use std::io::Write;
use std::path::Path;

/// Write a stereo 44.1 kHz / 16-bit WAV of all-zero PCM. Useful for proving
/// "decode but no signal" doesn't panic the fingerprinter.
pub(crate) fn write_silence_wav(path: &Path, seconds: u32) {
    write_pcm_wav(path, seconds, 2, |_, _, _| 0);
}

/// Write a mono 44.1 kHz / 16-bit WAV of a 440 Hz sine — short, deterministic,
/// and loud enough that Chromaprint's frame analysis emits hashes.
pub(crate) fn write_sine_wav(path: &Path, seconds: u32) {
    write_pcm_wav(path, seconds, 1, |frame, sample_rate, _| {
        let t = frame as f32 / sample_rate as f32;
        ((t * 440.0 * std::f32::consts::TAU).sin() * 30_000.0) as i16
    });
}

/// Write a stereo 44.1 kHz / 16-bit WAV of a deterministic "tune": a melody
/// of stepped notes with harmonics and a per-note envelope, plus a different
/// counter-line in each channel. Known-answer tests pin the fingerprint and
/// transcode output of this file, so it must have enough spectral movement
/// for Chromaprint to hash and enough channel difference to exercise the
/// stereo paths. Changing this function invalidates those pinned answers.
pub(crate) fn write_tune_wav(path: &Path, seconds: u32) {
    // Semitone offsets from A3 (220 Hz) for an eight-note phrase.
    const MELODY: [i32; 8] = [0, 3, 7, 12, 10, 7, 5, 3];
    write_pcm_wav(path, seconds, 2, |frame, sample_rate, channel| {
        let t = f64::from(frame) / f64::from(sample_rate);
        let note_len = 0.25;
        let idx = (t / note_len) as usize;
        let in_note = t - idx as f64 * note_len;
        let semis = MELODY[idx % MELODY.len()] + if channel == 1 { 7 } else { 0 };
        let f = 220.0 * 2f64.powf(f64::from(semis) / 12.0);
        let env = (-in_note * 6.0).exp();
        let tau = std::f64::consts::TAU;
        let tone = (t * f * tau).sin()
            + 0.5 * (t * 2.0 * f * tau).sin()
            + 0.25 * (t * 3.0 * f * tau).sin();
        // Slow bass line shared by both channels, an octave below every bar.
        let bass =
            0.4 * (t * 110.0 * 2f64.powf(f64::from((idx / 8) as i32 % 3 * 2) / 12.0) * tau).sin();
        ((tone * env * 0.5 + bass) * 12_000.0).round() as i16
    });
}

/// Helper: write a 44.1 kHz / 16-bit RIFF WAV using
/// `sample(frame, rate, channel)` to produce each sample.
fn write_pcm_wav(path: &Path, seconds: u32, channels: u16, sample: impl Fn(u32, u32, u16) -> i16) {
    let sample_rate: u32 = 44_100;
    let bits: u16 = 16;
    let frames = sample_rate * seconds;
    let data_size = frames * u32::from(channels) * u32::from(bits) / 8;
    let byte_rate = sample_rate * u32::from(channels) * u32::from(bits) / 8;
    let block_align = channels * bits / 8;
    let riff_size = 36 + data_size;

    let mut f = fs::File::create(path).unwrap();
    f.write_all(b"RIFF").unwrap();
    f.write_all(&riff_size.to_le_bytes()).unwrap();
    f.write_all(b"WAVE").unwrap();
    f.write_all(b"fmt ").unwrap();
    f.write_all(&16u32.to_le_bytes()).unwrap();
    f.write_all(&1u16.to_le_bytes()).unwrap();
    f.write_all(&channels.to_le_bytes()).unwrap();
    f.write_all(&sample_rate.to_le_bytes()).unwrap();
    f.write_all(&byte_rate.to_le_bytes()).unwrap();
    f.write_all(&block_align.to_le_bytes()).unwrap();
    f.write_all(&bits.to_le_bytes()).unwrap();
    f.write_all(b"data").unwrap();
    f.write_all(&data_size.to_le_bytes()).unwrap();

    let mut pcm = Vec::with_capacity(data_size as usize);
    for frame in 0..frames {
        for channel in 0..channels {
            let v = sample(frame, sample_rate, channel);
            pcm.extend_from_slice(&v.to_le_bytes());
        }
    }
    f.write_all(&pcm).unwrap();
}

/// A one-second MP3 encoded by ffmpeg under the temp dir, or `None` when
/// ffmpeg is not installed (the caller skips). MP3 is the container with
/// ID3v1 as a secondary tag; the WAV helpers cannot stand in for it.
pub(crate) fn ffmpeg_mp3(stem: &str) -> Option<std::path::PathBuf> {
    let path = std::env::temp_dir().join(format!("zytunes-{stem}-{}.mp3", std::process::id()));
    let _ = fs::remove_file(&path);
    let out = std::process::Command::new("ffmpeg")
        .args([
            "-y",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=1",
            "-q:a",
            "9",
        ])
        .arg(&path)
        .output()
        .ok()?;
    if !out.status.success() {
        eprintln!("ffmpeg cannot encode mp3 here; skipping");
        return None;
    }
    Some(path)
}
