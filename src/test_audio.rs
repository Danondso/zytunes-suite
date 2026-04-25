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
    write_pcm_wav(path, seconds, 2, |_, _| 0);
}

/// Write a mono 44.1 kHz / 16-bit WAV of a 440 Hz sine — short, deterministic,
/// and loud enough that Chromaprint's frame analysis emits hashes.
pub(crate) fn write_sine_wav(path: &Path, seconds: u32) {
    write_pcm_wav(path, seconds, 1, |frame, sample_rate| {
        let t = frame as f32 / sample_rate as f32;
        ((t * 440.0 * std::f32::consts::TAU).sin() * 30_000.0) as i16
    });
}

/// Helper: write a 44.1 kHz / 16-bit RIFF WAV using `sample(frame, rate)` to
/// produce each sample. The same value is written to all channels (the
/// fingerprint tests don't care about stereo separation).
fn write_pcm_wav(path: &Path, seconds: u32, channels: u16, sample: impl Fn(u32, u32) -> i16) {
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
        let v = sample(frame, sample_rate);
        for _ in 0..channels {
            pcm.extend_from_slice(&v.to_le_bytes());
        }
    }
    f.write_all(&pcm).unwrap();
}
