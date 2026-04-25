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
use lofty::config::ParseOptions;
use lofty::file::TaggedFileExt;
use lofty::probe::Probe;
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
    // Skip the properties parse — we only need the tag. For VBR MP3, the
    // properties pass scans frames across the whole file for duration, which
    // costs ~100x more than the tag read alone. Hot path on every cached
    // track that lacks a stored fingerprint.
    let tagged = Probe::open(path)
        .ok()?
        .options(ParseOptions::new().read_properties(false))
        .guess_file_type()
        .ok()?
        .read()
        .ok()?;
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
///
/// Internally panic-isolated: symphonia's AAC decoder and rusty-chromaprint's
/// audio processor each have known panic paths on edge-case audio (malformed
/// frames, channel-count mismatches, internal invariant violations). Without
/// isolation a single bad file kills the rayon scan via panic propagation
/// through `.collect()`, leaving the cache unsaved and forcing every launch
/// to start over. Panics are caught, logged, and converted to `None`.
pub fn compute_fingerprint(path: &Path) -> Option<String> {
    let path_buf = path.to_path_buf();
    // Per-file panics get absorbed silently — the scan-end summary reports
    // the total `fp_failed` count, which is the only number that matters
    // for "is the cache making progress." Listing each panicking file every
    // launch was just noise once we confirmed the panics are upstream
    // (symphonia AAC + rusty-chromaprint internal asserts).
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        compute_fingerprint_inner(&path_buf)
    }))
    .ok()
    .flatten()
}

fn compute_fingerprint_inner(path: &Path) -> Option<String> {
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
    let track_id = track.id;

    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .ok()?;

    // Initialise chromaprint lazily from the first decoded packet's spec, not
    // from `codec_params`. AAC tracks sometimes advertise stereo in container
    // metadata but decode mono frames (or vice versa); using `codec_params`
    // for `Fingerprinter::start` trips chromaprint's internal channel-count
    // assertion when a mismatch fires. The decoded spec is authoritative.
    let mut fp: Option<Fingerprinter> = None;
    let mut max_samples: u64 = 0;
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

        if fp.is_none() {
            let spec = *decoded.spec();
            let channels = spec.channels.count() as u32;
            if channels == 0 {
                return None;
            }
            let mut new_fp = Fingerprinter::new(&config);
            new_fp.start(spec.rate, channels).ok()?;
            fp = Some(new_fp);
            max_samples = u64::from(spec.rate) * u64::from(channels) * FINGERPRINT_SECONDS;
        }

        // SampleBuffer must be at least as large as the largest packet's
        // capacity, otherwise `copy_interleaved_ref` writes past the end.
        // M4A and other variable-frame containers can produce packets larger
        // than the first one, so grow the buffer on demand instead of
        // sizing-once-from-first-packet.
        let needed = decoded.capacity() as u64;
        let needs_alloc = sbuf.as_ref().is_none_or(|b| (b.capacity() as u64) < needed);
        if needs_alloc {
            sbuf = Some(SampleBuffer::new(needed, *decoded.spec()));
        }
        let buf = sbuf.as_mut().expect("sample buffer initialised above");
        buf.copy_interleaved_ref(decoded);
        let samples = buf.samples();
        if samples.is_empty() {
            continue;
        }
        fp.as_mut()
            .expect("fingerprinter initialised above")
            .consume(samples);
        samples_seen += samples.len() as u64;
        if samples_seen >= max_samples {
            break;
        }
    }

    let mut fp = fp?;
    fp.finish();
    let raw = fp.fingerprint();
    if raw.is_empty() {
        return None;
    }

    let compressed = FingerprintCompressor::from(&config).compress(raw);
    Some(URL_SAFE_NO_PAD.encode(compressed))
}

/// Read an embedded fingerprint if present, else compute one from the audio.
/// Inherits `compute_fingerprint`'s panic isolation on the compute fallback.
pub fn fingerprint_for(path: &Path) -> Option<String> {
    read_embedded_fingerprint(path).or_else(|| compute_fingerprint(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_audio::{write_silence_wav, write_sine_wav};
    use std::fs;

    /// Per-test scratch dir under one shared `zytunes-fp-tests/` parent so
    /// `cargo test` cleanup removes it all in one shot. Each test owns its
    /// subdir and tears it down on exit, mirroring the dirlib test pattern.
    fn fresh_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("zytunes-fp-tests").join(name);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
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
    fn compute_fingerprint_emits_something_for_real_audio() {
        let dir = fresh_dir("emits-real-audio");
        let path = dir.join("sine.wav");
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
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn compute_fingerprint_is_deterministic() {
        let dir = fresh_dir("deterministic");
        let path = dir.join("sine.wav");
        write_sine_wav(&path, 10);
        let a = compute_fingerprint(&path).unwrap();
        let b = compute_fingerprint(&path).unwrap();
        assert_eq!(a, b, "same file must produce identical fingerprints");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn compute_fingerprint_returns_none_for_non_audio() {
        let dir = fresh_dir("non-audio");
        let path = dir.join("not-audio.wav");
        fs::write(&path, b"this is not audio, it's just bytes").unwrap();
        assert!(compute_fingerprint(&path).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn compute_fingerprint_handles_multi_packet_audio() {
        // A 60-second WAV decodes across many packets. The earlier bug sized
        // SampleBuffer once from the first packet's capacity and wrote past
        // the end if a later packet was larger; this test makes sure long
        // files complete cleanly.
        let dir = fresh_dir("multi-packet");
        let path = dir.join("long.wav");
        write_sine_wav(&path, 60);
        let fp = compute_fingerprint(&path).expect("60s WAV must fingerprint");
        assert!(!fp.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn compute_fingerprint_handles_short_silence() {
        // Chromaprint needs enough signal to emit hashes; silence may produce
        // a degenerate fingerprint. The contract is only "no panic, no error"
        // — an empty result maps to None, which is acceptable.
        let dir = fresh_dir("short-silence");
        let path = dir.join("silence.wav");
        write_silence_wav(&path, 2);
        let _ = compute_fingerprint(&path); // must not panic
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_embedded_fingerprint_absent_returns_none() {
        let dir = fresh_dir("absent-tag");
        let path = dir.join("no-tag.wav");
        write_sine_wav(&path, 1);
        assert!(read_embedded_fingerprint(&path).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_embedded_fingerprint_returns_tag_value() {
        let dir = fresh_dir("tagged");
        let path = dir.join("tagged.wav");
        write_sine_wav(&path, 1);
        write_acoustid_id3_tag(
            &path,
            "Acoustid Fingerprint",
            "AQADtEmUaEmS5Ac",
            "Fingerprint Test",
        );

        let got = read_embedded_fingerprint(&path);
        assert_eq!(got.as_deref(), Some("AQADtEmUaEmS5Ac"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_embedded_fingerprint_matches_uppercase_key() {
        // Some taggers write `ACOUSTID_FINGERPRINT` as the ID3 TXXX
        // description; match it case-insensitively.
        let dir = fresh_dir("tagged-upper");
        let path = dir.join("tagged.wav");
        write_sine_wav(&path, 1);
        write_acoustid_id3_tag(
            &path,
            "ACOUSTID_FINGERPRINT",
            "AQADtEmUaEmS5Ac",
            "Upper Test",
        );

        let got = read_embedded_fingerprint(&path);
        assert_eq!(got.as_deref(), Some("AQADtEmUaEmS5Ac"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn fingerprint_for_prefers_embedded_tag_over_compute() {
        let dir = fresh_dir("prefers-tag");
        let path = dir.join("prefers.wav");
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
        let _ = fs::remove_dir_all(&dir);
    }
}
