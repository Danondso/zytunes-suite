//! Acoustic fingerprinting (Chromaprint / AcoustID).
//!
//! Two paths:
//!   1. Read a pre-computed `ACOUSTID_FINGERPRINT` tag — written by
//!      MusicBrainz Picard and the `fpcalc` CLI. Cheap; no decode.
//!   2. Compute one ourselves: symphonia → i16 PCM → `Fingerprinter` →
//!      URL-safe base64 of the standard Chromaprint compressed form.
//!
//! The printable encoding matches AcoustID's wire format (URL-safe base64,
//! no padding), so a fingerprint we compute compares byte-for-byte against
//! a Picard-tagged file that came from the same source.

use std::path::Path;

use base64::engine::{general_purpose::URL_SAFE_NO_PAD, Engine};
use lofty::file::TaggedFileExt;
use lofty::tag::{ItemKey, ItemValue};
use rusty_chromaprint::{Configuration, FingerprintCompressor, Fingerprinter};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// Tag key names Picard / fpcalc write for the Chromaprint fingerprint. Both
/// ID3 `TXXX:Acoustid Fingerprint` and Vorbis `ACOUSTID_FINGERPRINT` surface
/// as `ItemKey::Unknown(name)` in lofty 0.22 — there's no dedicated variant —
/// so we match on the stored name case-insensitively.
const ACOUSTID_TAG_KEYS: &[&str] = &["Acoustid Fingerprint", "ACOUSTID_FINGERPRINT"];

/// Seconds of audio to feed the fingerprinter. Chromaprint's AcoustID-default
/// (Test2) configuration summarises the first ~2 minutes of a track; decoding
/// further is wasted I/O that doesn't change the fingerprint.
const FINGERPRINT_SECONDS: u64 = 120;

/// Read a pre-computed acoustic fingerprint from the file's tags, if any.
///
/// Matches the tag name MusicBrainz Picard / fpcalc write:
///   - Vorbis Comment / MP4: `ACOUSTID_FINGERPRINT`
///   - ID3v2: `TXXX:Acoustid Fingerprint`
///
/// Returns the stored string verbatim — it's opaque to callers.
pub fn read_embedded_fingerprint(path: &Path) -> Option<String> {
    let tagged = lofty::probe::read_from_path(path).ok()?;
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag())?;

    for item in tag.items() {
        let ItemKey::Unknown(name) = item.key() else {
            continue;
        };
        if !ACOUSTID_TAG_KEYS
            .iter()
            .any(|k| name.eq_ignore_ascii_case(k))
        {
            continue;
        }
        if let ItemValue::Text(v) = item.value() {
            if !v.is_empty() {
                return Some(v.clone());
            }
        }
    }
    None
}

/// Compute an acoustic fingerprint for `path`.
///
/// Decodes the first `FINGERPRINT_SECONDS` of audio with symphonia, feeds it
/// to the AcoustID-default (Test2) fingerprinter, and returns the URL-safe
/// base64 form. Returns `None` on any decode or fingerprint failure —
/// fingerprinting is best-effort; a missing fingerprint is not a scan error.
pub fn compute_fingerprint(path: &Path) -> Option<String> {
    let config = Configuration::preset_test2();

    let file = std::fs::File::open(path).ok()?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .ok()?;
    let mut format = probed.format;

    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)?
        .clone();
    let sample_rate = track.codec_params.sample_rate?;
    let channels = track.codec_params.channels.map(|c| c.count()).unwrap_or(2) as u32;
    if channels == 0 {
        return None;
    }
    let track_id = track.id;

    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .ok()?;

    let mut fp = Fingerprinter::new(&config);
    fp.start(sample_rate, channels).ok()?;

    // Cap on interleaved sample count (frames × channels).
    let max_samples = u64::from(sample_rate) * u64::from(channels) * FINGERPRINT_SECONDS;
    let mut samples_seen: u64 = 0;
    let mut sbuf: Option<SampleBuffer<i16>> = None;

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymphoniaError::IoError(ref e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break
            }
            Err(_) => break,
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(_) => continue,
        };

        if sbuf.is_none() {
            sbuf = Some(SampleBuffer::new(
                decoded.capacity() as u64,
                *decoded.spec(),
            ));
        }
        let buf = sbuf.as_mut().expect("sample buffer initialised above");
        buf.copy_interleaved_ref(decoded);
        let samples = buf.samples();
        if samples.is_empty() {
            continue;
        }
        fp.consume(samples);
        samples_seen += samples.len() as u64;
        if samples_seen >= max_samples {
            break;
        }
    }

    fp.finish();
    let raw = fp.fingerprint();
    if raw.is_empty() {
        return None;
    }

    let compressed = FingerprintCompressor::from(&config).compress(raw);
    Some(URL_SAFE_NO_PAD.encode(compressed))
}

/// Read an embedded fingerprint if present, else compute one from the audio.
pub fn fingerprint_for(path: &Path) -> Option<String> {
    read_embedded_fingerprint(path).or_else(|| compute_fingerprint(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    fn temp_file(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("zytunes-fp-tests");
        let _ = fs::create_dir_all(&dir);
        dir.join(name)
    }

    fn write_silence_wav(path: &Path, seconds: u32) {
        // Minimal PCM WAV: 44.1 kHz, stereo, 16-bit, all-zero samples.
        let sample_rate: u32 = 44_100;
        let channels: u16 = 2;
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
        // All-zero PCM — writing a big zero buffer.
        let zeros = vec![0u8; data_size as usize];
        f.write_all(&zeros).unwrap();
    }

    fn write_sine_wav(path: &Path, seconds: u32) {
        let sample_rate: u32 = 44_100;
        let channels: u16 = 1;
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

        // 440 Hz sine so chromaprint actually has signal to latch onto.
        let mut pcm = Vec::with_capacity(data_size as usize);
        for i in 0..frames {
            let t = i as f32 / sample_rate as f32;
            let s = (t * 440.0 * std::f32::consts::TAU).sin();
            let v = (s * 30_000.0) as i16;
            pcm.extend_from_slice(&v.to_le_bytes());
        }
        f.write_all(&pcm).unwrap();
    }

    #[test]
    fn compute_fingerprint_emits_something_for_real_audio() {
        let path = temp_file("sine.wav");
        write_sine_wav(&path, 10);
        let fp = compute_fingerprint(&path);
        let fp = fp.expect("should produce a fingerprint for 10s of audio");
        // URL-safe base64, no padding; Chromaprint compressed form is always
        // > 0 bytes for any non-empty signal.
        assert!(!fp.is_empty());
        assert!(!fp.contains('='), "URL-safe-no-pad encoding drops '='");
        assert!(
            !fp.contains('+') && !fp.contains('/'),
            "URL-safe encoding uses '-' and '_' not '+/'"
        );
    }

    #[test]
    fn compute_fingerprint_is_deterministic() {
        let path = temp_file("sine-det.wav");
        write_sine_wav(&path, 10);
        let a = compute_fingerprint(&path).unwrap();
        let b = compute_fingerprint(&path).unwrap();
        assert_eq!(a, b, "same file must produce identical fingerprints");
    }

    #[test]
    fn compute_fingerprint_returns_none_for_non_audio() {
        let path = temp_file("not-audio.wav");
        fs::write(&path, b"this is not audio, it's just bytes").unwrap();
        assert!(compute_fingerprint(&path).is_none());
    }

    #[test]
    fn compute_fingerprint_handles_short_silence() {
        // Chromaprint needs enough signal to emit hashes; silence may produce
        // a degenerate fingerprint. The contract is only "no panic, no error"
        // — an empty result maps to None, which is acceptable.
        let path = temp_file("silence.wav");
        write_silence_wav(&path, 2);
        let _ = compute_fingerprint(&path); // must not panic
    }

    #[test]
    fn read_embedded_fingerprint_absent_returns_none() {
        let path = temp_file("no-tag.wav");
        write_sine_wav(&path, 1);
        assert!(read_embedded_fingerprint(&path).is_none());
    }

    /// Write an ID3v2 tag with the given `ACOUSTID_FINGERPRINT`-style
    /// extended-text frame into a WAV file (WAV permits an embedded `id3`
    /// chunk). Lofty's default `primary_tag_type()` for WAV is RIFF INFO,
    /// which has 4-char FourCC keys and silently drops unknown names — so
    /// tests exercising the `ACOUSTID_FINGERPRINT` path must force ID3v2.
    fn write_acoustid_id3_tag(path: &Path, frame_name: &str, value: &str, title: &str) {
        use id3::frame::ExtendedText;
        use id3::{Tag, TagLike, Version};

        let mut tag = Tag::new();
        tag.set_title(title);
        tag.add_frame(ExtendedText {
            description: frame_name.to_string(),
            value: value.to_string(),
        });
        tag.write_to_path(path, Version::Id3v24).unwrap();
    }

    #[test]
    fn read_embedded_fingerprint_returns_tag_value() {
        let path = temp_file("tagged.wav");
        write_sine_wav(&path, 1);
        write_acoustid_id3_tag(
            &path,
            "Acoustid Fingerprint",
            "AQADtEmUaEmS5Ac",
            "Fingerprint Test",
        );

        let got = read_embedded_fingerprint(&path);
        assert_eq!(got.as_deref(), Some("AQADtEmUaEmS5Ac"));
    }

    #[test]
    fn read_embedded_fingerprint_matches_uppercase_key() {
        // Some taggers write `ACOUSTID_FINGERPRINT` as the ID3 TXXX
        // description; match it case-insensitively.
        let path = temp_file("tagged-upper.wav");
        write_sine_wav(&path, 1);
        write_acoustid_id3_tag(
            &path,
            "ACOUSTID_FINGERPRINT",
            "AQADtEmUaEmS5Ac",
            "Upper Test",
        );

        let got = read_embedded_fingerprint(&path);
        assert_eq!(got.as_deref(), Some("AQADtEmUaEmS5Ac"));
    }

    #[test]
    fn fingerprint_for_prefers_embedded_tag_over_compute() {
        let path = temp_file("prefers-tag.wav");
        write_sine_wav(&path, 5);
        // An obviously fake value that could never come out of compute().
        write_acoustid_id3_tag(
            &path,
            "ACOUSTID_FINGERPRINT",
            "SENTINEL-NOT-A-REAL-FP",
            "Tag Beats Compute",
        );

        let got = fingerprint_for(&path).unwrap();
        assert_eq!(
            got, "SENTINEL-NOT-A-REAL-FP",
            "fingerprint_for must short-circuit on the embedded tag"
        );
    }
}
