//! Audio/video transcoding pipeline for device sync.
//!
//! Everything between "local file in a format the device may not accept" and
//! "file ready to upload" lives here: format-decision helpers, the pure-Rust
//! MP3 path (symphonia decode → LAME encode → Xing header patch), the
//! ffmpeg-backed FLAC→ALAC and video→WMV paths, and the metadata carry-over
//! that keeps tags and album art intact across a transcode.

use crate::device::DeviceCapabilities;
use crate::mtp::{self, DeviceSession};
use std::path::Path;

/// Path of a unique temp directory for transcoded files (includes PID to
/// avoid collisions). Only builds the path — the transcode helpers create
/// the directory on first use.
pub fn make_transcode_temp_dir() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("zytunes-transcode-{}", std::process::id()))
}

/// Check if a file needs transcoding for the target device.
pub fn needs_transcoding(path: &str, supported_formats: &[&str]) -> bool {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    !supported_formats.contains(&ext.as_str())
}

/// Check if a video file needs transcoding for the Zune (only WMV is native).
pub fn needs_video_transcoding(path: &str) -> bool {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    ext != "wmv"
}

/// Check whether ffmpeg is available on the system.
pub fn check_ffmpeg_available() -> bool {
    std::process::Command::new("ffmpeg")
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Transcode a video file to WMV format for the Zune via ffmpeg.
///
/// Uses wmv2 video codec at 320x240 and wmav2 audio — the Zune's native
/// playback format. Returns the path to the output WMV file.
pub fn transcode_to_wmv(input: &str, temp_dir: &Path) -> Result<String, String> {
    std::fs::create_dir_all(temp_dir).map_err(|e| format!("Cannot create temp dir: {e}"))?;

    let stem = Path::new(input)
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy();
    let output = temp_dir.join(format!("{stem}.wmv"));

    let result = std::process::Command::new("ffmpeg")
        .args([
            "-y",
            "-i",
            input,
            "-c:v",
            "wmv2",
            "-b:v",
            "768k",
            "-vf",
            "scale=320:240:force_original_aspect_ratio=decrease,pad=320:240:(ow-iw)/2:(oh-ih)/2",
            "-c:a",
            "wmav2",
            "-b:a",
            "128k",
            "-ar",
            "44100",
        ])
        .arg(output.to_str().ok_or("output path not UTF-8")?)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| format!("Failed to run ffmpeg: {e}"))?;

    if !result.status.success() {
        let stderr = String::from_utf8_lossy(&result.stderr);
        return Err(format!("ffmpeg transcode failed: {}", stderr));
    }

    Ok(output.to_string_lossy().into_owned())
}

/// Transcode a video if needed, then import via the device session.
pub fn transcode_and_import_video(
    session: &mut dyn DeviceSession,
    local_path: &str,
    temp_dir: &Path,
) -> Result<u64, String> {
    let upload_path = if needs_video_transcoding(local_path) {
        transcode_to_wmv(local_path, temp_dir)?
    } else {
        local_path.to_string()
    };
    let filename = Path::new(&upload_path)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let data = std::fs::read(&upload_path).map_err(|e| format!("Read video: {e}"))?;
    session.import_video(&filename, &data)
}

/// Transcode if needed, then import via the device session.
pub fn transcode_and_import(
    session: &mut dyn DeviceSession,
    local_path: &str,
    temp_dir: &Path,
    caps: &DeviceCapabilities,
    meta: Option<&mtp::TrackMeta>,
) -> Result<u64, String> {
    let upload_path = transcode_for_device(local_path, temp_dir, caps)?;
    session.import_track(&upload_path, meta)
}

/// Pick the right transcode path for a source file → device. Three tiers,
/// in order:
///
/// 1. **Lossless promotion** — when the device declares
///    `lossless_target = Some("alac")` and the source is a FLAC, transcode
///    to ALAC via ffmpeg so the lossless tier is preserved on device.
/// 2. **Lossy fallback** — when the source is in a non-native lossy
///    format, the existing pure-Rust `transcode_to_mp3` path kicks in.
/// 3. **Passthrough** — when the source is already in a format the device
///    accepts, no work happens.
///
/// Extracted from `transcode_and_import` for testability and so the
/// command-line `push` path can reuse it.
pub fn transcode_for_device(
    local_path: &str,
    temp_dir: &Path,
    caps: &DeviceCapabilities,
) -> Result<String, String> {
    if let Some(lossless_target) = caps.lossless_target {
        if is_flac(local_path) && lossless_target == "alac" {
            return transcode_flac_to_alac(local_path, temp_dir);
        }
    }
    if needs_transcoding(local_path, caps.supported_formats) {
        return transcode_to_mp3(local_path, temp_dir, caps.max_art_dimensions);
    }
    Ok(local_path.to_string())
}

fn is_flac(path: &str) -> bool {
    std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("flac"))
}

/// Transcode FLAC to ALAC via ffmpeg, preserving metadata.
///
/// Output container is `.m4a` (ALAC inside MP4) — that's the form iPod
/// firmware expects. Uses `-c:a alac` (ffmpeg's native ALAC encoder; no
/// external library required). `-vn` strips embedded artwork from the
/// transcoded copy — the iPod doesn't read embedded art (artwork goes via
/// ArtworkDB, populated separately).
pub fn transcode_flac_to_alac(input: &str, temp_dir: &Path) -> Result<String, String> {
    if !check_ffmpeg_available() {
        return Err("ffmpeg is required for FLAC→ALAC transcoding".into());
    }
    std::fs::create_dir_all(temp_dir).map_err(|e| format!("Cannot create temp dir: {e}"))?;
    let input_path = std::path::Path::new(input);
    let stem = input_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("track");
    let output = temp_dir.join(format!("{stem}.m4a"));

    let status = std::process::Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "warning",
            "-y",
            "-i",
            input,
            "-c:a",
            "alac",
            "-vn", // drop embedded art — iPod uses ArtworkDB
            output.to_str().ok_or("output path not UTF-8")?,
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| format!("failed to spawn ffmpeg: {e}"))?;

    if !status.status.success() {
        let stderr = String::from_utf8_lossy(&status.stderr);
        return Err(format!(
            "ffmpeg FLAC→ALAC failed ({:?}): {}",
            status.status.code(),
            stderr
                .lines()
                .filter(|l| !l.is_empty())
                .collect::<Vec<_>>()
                .join(" / ")
        ));
    }

    Ok(output.to_string_lossy().into_owned())
}

/// Parsed MPEG audio frame header fields needed for Xing-tag patching.
struct Mp3FrameHeader {
    /// Whole frame length in bytes, including the 4-byte header.
    frame_len: usize,
    /// MPEG-1 (true) vs MPEG-2/2.5 (false) — decides side-info size.
    mpeg1: bool,
    /// Mono channel mode — decides side-info size.
    mono: bool,
}

/// Parse an MPEG-1/2/2.5 Layer III frame header at the start of `b`.
/// Returns `None` for anything that is not a plain Layer III frame with a
/// defined bitrate (free-format and invalid indices are rejected).
fn parse_mp3_frame_header(b: &[u8]) -> Option<Mp3FrameHeader> {
    if b.len() < 4 || b[0] != 0xff || (b[1] & 0xe0) != 0xe0 {
        return None;
    }
    let version = (b[1] >> 3) & 0x3; // 3 = MPEG1, 2 = MPEG2, 0 = MPEG2.5
    let layer = (b[1] >> 1) & 0x3; // 1 = Layer III
    if layer != 1 || version == 1 {
        return None;
    }
    let bitrate_idx = (b[2] >> 4) as usize;
    let sr_idx = ((b[2] >> 2) & 0x3) as usize;
    if bitrate_idx == 0 || bitrate_idx == 15 || sr_idx == 3 {
        return None;
    }
    const BITRATE_V1: [u32; 15] = [
        0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
    ];
    const BITRATE_V2: [u32; 15] = [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160];
    const SAMPLE_RATE_V1: [u32; 3] = [44_100, 48_000, 32_000];
    let mpeg1 = version == 3;
    let sample_rate = match version {
        3 => SAMPLE_RATE_V1[sr_idx],
        2 => SAMPLE_RATE_V1[sr_idx] / 2,
        _ => SAMPLE_RATE_V1[sr_idx] / 4,
    };
    let bitrate = if mpeg1 {
        BITRATE_V1[bitrate_idx]
    } else {
        BITRATE_V2[bitrate_idx]
    } * 1000;
    let padding = ((b[2] >> 1) & 1) as usize;
    let per_frame = if mpeg1 { 144 } else { 72 };
    let frame_len = per_frame * bitrate as usize / sample_rate as usize + padding;
    let mono = (b[3] >> 6) == 0b11;
    Some(Mp3FrameHeader {
        frame_len,
        mpeg1,
        mono,
    })
}

/// Overwrite LAME's reserved VBR-tag placeholder with a valid Xing header.
///
/// LAME (with `bWriteVbrTag` on, its default) emits an all-zero placeholder
/// frame at the start of the stream and expects the caller to rewrite it
/// after encoding — the C file-based flow does this via
/// `lame_get_lametag_frame()`, which the Rust wrapper does not expose. If the
/// placeholder is left zeroed, the stream carries no Xing data at all, and
/// players fall back to estimating duration as
/// `stream bytes ÷ first-frame bitrate`. Against the placeholder's nominal
/// 128 kbps, typical VBR NearBest music (~250 kbps average) reads as roughly
/// twice its real length — the Zune then shows a doubled duration and replays
/// the audio to fill the phantom tail, and the iPod DB records the same bad
/// estimate via lofty.
///
/// Returns `false` (leaving `mp3` untouched) when the stream doesn't start
/// with a zeroed placeholder or any frame fails to parse — in that case the
/// output is exactly what we shipped before this fix, never corrupted.
fn patch_xing_header(mp3: &mut [u8]) -> bool {
    let Some(first) = parse_mp3_frame_header(mp3) else {
        return false;
    };
    if mp3.len() < first.frame_len {
        return false;
    }
    // Only patch a genuine placeholder: payload must be all zeros.
    if mp3[4..first.frame_len].iter().any(|&b| b != 0) {
        return false;
    }
    let side_info_len = match (first.mpeg1, first.mono) {
        (true, true) => 17,
        (true, false) => 32,
        (false, true) => 9,
        (false, false) => 17,
    };
    // "Xing" + flags + frame count + byte count + 100-entry TOC.
    if 4 + side_info_len + 112 > first.frame_len {
        return false;
    }

    // Walk the whole stream to collect frame offsets. Bail on any parse
    // failure — a wrong TOC is worse than no TOC.
    let mut offsets: Vec<usize> = Vec::new();
    let mut pos = 0usize;
    while pos < mp3.len() {
        let Some(h) = parse_mp3_frame_header(&mp3[pos..]) else {
            return false;
        };
        if pos + h.frame_len > mp3.len() {
            return false;
        }
        offsets.push(pos);
        pos += h.frame_len;
    }
    // The placeholder itself is not an audio frame.
    let audio_frames = (offsets.len().saturating_sub(1)) as u32;
    if audio_frames == 0 {
        return false;
    }
    let total_bytes = mp3.len() as u32;

    let tag_start = 4 + side_info_len;
    mp3[tag_start..tag_start + 4].copy_from_slice(b"Xing");
    // Flags: FRAMES | BYTES | TOC.
    mp3[tag_start + 4..tag_start + 8].copy_from_slice(&7u32.to_be_bytes());
    mp3[tag_start + 8..tag_start + 12].copy_from_slice(&audio_frames.to_be_bytes());
    mp3[tag_start + 12..tag_start + 16].copy_from_slice(&total_bytes.to_be_bytes());
    for i in 0..100usize {
        let frame_idx = 1 + (i * audio_frames as usize) / 100;
        let off = offsets[frame_idx.min(offsets.len() - 1)];
        let scaled = (off as u64 * 256 / u64::from(total_bytes)).min(255) as u8;
        mp3[tag_start + 16 + i] = scaled;
    }
    true
}

/// Byte index of the end of the last frame in `samples` that contains any
/// non-zero value. Returns 0 if the entire slice is digital silence.
fn last_nonzero_frame_end(samples: &[f32], channels: usize) -> usize {
    if channels == 0 {
        return 0;
    }
    let last_nonzero = samples
        .iter()
        .rposition(|&s| s != 0.0)
        .map(|i| i + 1)
        .unwrap_or(0);
    last_nonzero.div_ceil(channels) * channels
}

fn encode_pcm(
    encoder: &mut mp3lame_encoder::Encoder,
    samples: &[f32],
    channels: usize,
    output: &mut Vec<u8>,
) -> Result<(), String> {
    if samples.is_empty() {
        return Ok(());
    }
    let frames = samples.len() / channels;
    output.reserve(mp3lame_encoder::max_required_buffer_size(frames));
    let res = match channels {
        1 => encoder.encode_to_vec(mp3lame_encoder::MonoPcm(samples), output),
        2 => encoder.encode_to_vec(mp3lame_encoder::InterleavedPcm(samples), output),
        n => return Err(format!("Unsupported channel count: {n}")),
    };
    res.map(|_| ()).map_err(|e| format!("LAME encode: {e:?}"))
}

/// Transcode a file to MP3 using pure Rust libraries.
/// Preserves metadata and resizes album art to the given dimensions (or 200x200 by default).
pub fn transcode_to_mp3(
    input: &str,
    temp_dir: &Path,
    max_art_dimensions: Option<(u32, u32)>,
) -> Result<String, String> {
    use id3::TagLike;
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::DecoderOptions;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    std::fs::create_dir_all(temp_dir).map_err(|e| format!("Cannot create temp dir: {e}"))?;
    let input_path = Path::new(input);
    let stem = input_path.file_stem().unwrap_or_default().to_string_lossy();
    let output = temp_dir.join(format!("{stem}.mp3"));

    // 1. Read metadata and album art with lofty
    let meta = read_lofty_metadata(input)?;

    // 2. Decode audio with symphonia
    let file = std::fs::File::open(input).map_err(|e| format!("Cannot open {input}: {e}"))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = input_path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| format!("Failed to probe {input}: {e}"))?;

    let mut format = probed.format;
    let track = format
        .default_track()
        .ok_or("No audio track found")?
        .clone();

    let sample_rate = track
        .codec_params
        .sample_rate
        .ok_or("Unknown sample rate")?;
    let channels = track.codec_params.channels.map(|c| c.count()).unwrap_or(2);

    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| format!("Failed to create decoder: {e}"))?;

    // 3. Encode to MP3 with LAME
    let mut mp3_builder = mp3lame_encoder::Builder::new().ok_or("Failed to create LAME builder")?;
    mp3_builder
        .set_num_channels(channels as u8)
        .map_err(|e| format!("LAME set channels: {e:?}"))?;
    mp3_builder
        .set_sample_rate(sample_rate)
        .map_err(|e| format!("LAME set sample rate: {e:?}"))?;
    mp3_builder
        .set_vbr_mode(mp3lame_encoder::VbrMode::Mtrh)
        .map_err(|e| format!("LAME set VBR mode: {e:?}"))?;
    mp3_builder
        .set_vbr_quality(mp3lame_encoder::Quality::NearBest)
        .map_err(|e| format!("LAME set VBR quality: {e:?}"))?;
    let mut mp3_encoder = mp3_builder
        .build()
        .map_err(|e| format!("LAME build: {e:?}"))?;

    if channels == 0 || channels > 2 {
        return Err(format!(
            "Cannot transcode {channels}-channel audio; only mono and stereo are supported"
        ));
    }

    let mut mp3_data = Vec::new();
    let mut sample_buf: Option<SampleBuffer<f32>> = None;
    // Trailing run of zero samples. Held rather than encoded so that digital
    // silence at the tail of the stream is dropped at EOF. symphonia's isomp4
    // demuxer parses M4A edit-list atoms but does not apply them, so silence
    // the source's edit list would have trimmed otherwise bleeds into LAME.
    let mut trailing_zeros: Vec<f32> = Vec::new();
    let mut encoded_any = false;

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(symphonia::core::errors::Error::IoError(ref e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(e) => return Err(format!("Decode error: {e}")),
        };

        if packet.track_id() != track.id {
            continue;
        }

        let decoded = decoder
            .decode(&packet)
            .map_err(|e| format!("Decode packet: {e}"))?;

        let spec = *decoded.spec();
        let duration = decoded.capacity() as u64;

        if sample_buf.is_none() {
            sample_buf = Some(SampleBuffer::new(duration, spec));
        }
        let sbuf = sample_buf.as_mut().unwrap();
        sbuf.copy_interleaved_ref(decoded);

        let samples = sbuf.samples();
        let split_at = last_nonzero_frame_end(samples, channels);
        let (audio, trailing) = samples.split_at(split_at);

        if !audio.is_empty() {
            if !trailing_zeros.is_empty() {
                encode_pcm(&mut mp3_encoder, &trailing_zeros, channels, &mut mp3_data)?;
                trailing_zeros.clear();
            }
            encode_pcm(&mut mp3_encoder, audio, channels, &mut mp3_data)?;
            encoded_any = true;
        }

        trailing_zeros.extend_from_slice(trailing);
    }

    if encoded_any {
        // Held trailing zeros were genuinely at the tail — drop them.
        drop(trailing_zeros);
    } else {
        // Entire stream was digital silence. Preserve it so downstream tag
        // writing and LAME's flush both have valid state to work with.
        encode_pcm(&mut mp3_encoder, &trailing_zeros, channels, &mut mp3_data)?;
    }

    // FlushGap is the correct end-of-stream flush for a standalone track
    // (pads the final frame with zeros, allows id3v1). FlushNoGap is for
    // gapless concatenation and leaves the final frame padded with ancillary
    // data, which some decoders (including the Zune) mishandle.
    mp3_encoder
        .flush_to_vec::<mp3lame_encoder::FlushGap>(&mut mp3_data)
        .map_err(|e| format!("LAME flush: {e:?}"))?;

    // Fill in the Xing/VBR header LAME reserved at the head of the stream.
    // Best-effort: a `false` return leaves the stream as-is (playable, but
    // duration estimates fall back to first-frame bitrate).
    patch_xing_header(&mut mp3_data);

    // 4. Write the MP3 file
    std::fs::write(&output, &mp3_data).map_err(|e| format!("Write MP3: {e}"))?;

    // 5. Write ID3v2.3 tags and album art
    let mut tag = id3::Tag::new();
    if !meta.artist.is_empty() {
        tag.set_artist(&meta.artist);
    }
    if !meta.album.is_empty() {
        tag.set_album(&meta.album);
    }
    if !meta.title.is_empty() {
        tag.set_title(&meta.title);
    }
    if meta.track_num > 0 {
        tag.set_track(meta.track_num);
    }
    if !meta.genre.is_empty() {
        tag.set_genre(&meta.genre);
    }

    // Resize and embed album art (dimensions from device capabilities, default 200x200)
    if let Some(art_data) = meta.album_art {
        if let Ok(img) = image::load_from_memory(&art_data) {
            let (art_w, art_h) = max_art_dimensions.unwrap_or((200, 200));
            let resized = img.resize_exact(art_w, art_h, image::imageops::FilterType::Lanczos3);
            let mut jpeg_buf = std::io::Cursor::new(Vec::new());
            if resized
                .write_to(&mut jpeg_buf, image::ImageFormat::Jpeg)
                .is_ok()
            {
                tag.add_frame(id3::frame::Picture {
                    mime_type: "image/jpeg".to_string(),
                    picture_type: id3::frame::PictureType::CoverFront,
                    description: String::new(),
                    data: jpeg_buf.into_inner(),
                });
            }
        }
    }

    tag.write_to_path(&output, id3::Version::Id3v23)
        .map_err(|e| format!("Write ID3 tags: {e}"))?;

    Ok(output.to_string_lossy().to_string())
}

/// Metadata extracted from an audio file for transcoding.
struct AudioMetadata {
    artist: String,
    album: String,
    title: String,
    track_num: u32,
    genre: String,
    album_art: Option<Vec<u8>>,
}

/// Read metadata and album art from an audio file using lofty.
fn read_lofty_metadata(path: &str) -> Result<AudioMetadata, String> {
    use lofty::file::TaggedFileExt;
    use lofty::tag::Accessor;

    let tagged = lofty::probe::read_from_path(path)
        .map_err(|e| format!("Failed to read metadata from {path}: {e}"))?;

    let tag = match tagged.primary_tag().or_else(|| tagged.first_tag()) {
        Some(t) => t,
        None => {
            let stem = Path::new(path)
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            return Ok(AudioMetadata {
                artist: String::new(),
                album: String::new(),
                title: stem,
                track_num: 0,
                genre: String::new(),
                album_art: None,
            });
        }
    };

    Ok(AudioMetadata {
        artist: tag.artist().map(|s| s.to_string()).unwrap_or_default(),
        album: tag.album().map(|s| s.to_string()).unwrap_or_default(),
        title: tag.title().map(|s| s.to_string()).unwrap_or_else(|| {
            Path::new(path)
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string()
        }),
        track_num: tag.track().unwrap_or(0),
        genre: tag.genre().map(|s| s.to_string()).unwrap_or_default(),
        album_art: tag.pictures().first().map(|pic| pic.data().to_vec()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::{DeviceCapabilities, DeviceFamily};
    use crate::ZUNE_NATIVE_FORMATS as ZUNE_FORMATS;
    use id3::TagLike;

    fn test_caps() -> DeviceCapabilities {
        DeviceCapabilities {
            family: DeviceFamily::Zune,
            supported_formats: ZUNE_FORMATS,
            transcode_target: "mp3",
            lossless_target: None,
            music_root: "/Music",
            max_art_dimensions: Some((200, 200)),
        }
    }

    fn ipod_caps_with_lossless() -> DeviceCapabilities {
        DeviceCapabilities {
            family: DeviceFamily::Ipod,
            supported_formats: &["mp3", "m4a", "aac", "alac", "wav", "aiff"],
            transcode_target: "mp3",
            lossless_target: Some("alac"),
            music_root: ":iPod_Control:Music",
            max_art_dimensions: None,
        }
    }

    #[test]
    fn needs_transcoding_by_extension() {
        // Native formats — no transcoding
        assert!(!needs_transcoding("song.mp3", ZUNE_FORMATS));
        assert!(!needs_transcoding("song.wma", ZUNE_FORMATS));
        assert!(!needs_transcoding("song.aac", ZUNE_FORMATS));
        // Non-native — needs transcoding
        assert!(needs_transcoding("song.flac", ZUNE_FORMATS));
        assert!(needs_transcoding("song.m4a", ZUNE_FORMATS));
        assert!(needs_transcoding("song.ogg", ZUNE_FORMATS));
        assert!(needs_transcoding("song.wav", ZUNE_FORMATS));
    }

    #[test]
    fn temp_dir_includes_pid() {
        let dir = make_transcode_temp_dir();
        assert!(dir
            .to_str()
            .unwrap()
            .contains(&std::process::id().to_string()));
    }

    // -- needs_video_transcoding --

    #[test]
    fn wmv_does_not_need_transcoding() {
        assert!(!needs_video_transcoding("movie.wmv"));
        assert!(!needs_video_transcoding("MOVIE.WMV"));
        assert!(!needs_video_transcoding("/path/to/file.Wmv"));
    }

    #[test]
    fn non_wmv_needs_transcoding() {
        assert!(needs_video_transcoding("movie.mp4"));
        assert!(needs_video_transcoding("movie.avi"));
        assert!(needs_video_transcoding("movie.mpeg"));
        assert!(needs_video_transcoding("movie.mpg"));
        assert!(needs_video_transcoding("movie.mkv"));
    }

    // -- Transcoding decision tests --

    #[test]
    fn is_flac_recognises_extension_case_insensitively() {
        assert!(is_flac("song.flac"));
        assert!(is_flac("/abs/path/song.FLAC"));
        assert!(is_flac("song.Flac"));
        assert!(!is_flac("song.mp3"));
        assert!(!is_flac("noext"));
        assert!(!is_flac(""));
    }

    #[test]
    fn transcode_for_device_passes_through_native_format() {
        let caps = test_caps();
        let temp = std::env::temp_dir();
        let path = "/somewhere/song.mp3";
        assert_eq!(transcode_for_device(path, &temp, &caps).unwrap(), path);
    }

    #[test]
    fn transcode_for_device_no_lossless_target_falls_through_to_mp3_for_flac() {
        // Zune has no lossless_target → FLAC should hit the MP3 path. We
        // can't run the real transcode without a sample FLAC, but we can
        // exercise the dispatch by pointing at a missing file and
        // confirming the error message comes from `transcode_to_mp3`, not
        // the ALAC branch.
        let caps = test_caps();
        let temp = std::env::temp_dir();
        let err = transcode_for_device("/nope/missing.flac", &temp, &caps).unwrap_err();
        // Hits the MP3 path → error mentions decode/open of the file
        assert!(
            !err.contains("ALAC") && !err.contains("alac"),
            "should not have hit the ALAC branch; got {err:?}"
        );
    }

    #[test]
    fn transcode_for_device_ipod_routes_flac_through_alac_branch() {
        // iPod has `lossless_target = Some("alac")`. With no ffmpeg in
        // the test env, the ALAC branch fails fast with a recognisable
        // error so we know we dispatched correctly.
        let caps = ipod_caps_with_lossless();
        let temp = std::env::temp_dir();
        let result = transcode_for_device("/nope/missing.flac", &temp, &caps);
        match result {
            Err(e) if e.contains("ffmpeg") => {} // expected — either spawn-fail or process-fail
            Err(other) => panic!("expected ffmpeg-related error, got {other:?}"),
            Ok(p) => panic!("unexpectedly succeeded with {p:?}"),
        }
    }

    #[test]
    fn transcode_for_device_ipod_passes_through_native_mp3() {
        let caps = ipod_caps_with_lossless();
        let temp = std::env::temp_dir();
        let path = "/somewhere/song.mp3";
        assert_eq!(transcode_for_device(path, &temp, &caps).unwrap(), path);
    }

    #[test]
    fn needs_transcoding_native_formats() {
        assert!(!needs_transcoding("song.mp3", ZUNE_FORMATS));
        assert!(!needs_transcoding("song.wma", ZUNE_FORMATS));
        assert!(!needs_transcoding("song.aac", ZUNE_FORMATS));
        assert!(!needs_transcoding("SONG.MP3", ZUNE_FORMATS)); // case insensitive
    }

    #[test]
    fn needs_transcoding_non_native_formats() {
        assert!(needs_transcoding("song.flac", ZUNE_FORMATS));
        assert!(needs_transcoding("song.ogg", ZUNE_FORMATS));
        assert!(needs_transcoding("song.wav", ZUNE_FORMATS));
        assert!(needs_transcoding("song.m4a", ZUNE_FORMATS));
        assert!(needs_transcoding("song.opus", ZUNE_FORMATS));
        assert!(needs_transcoding("song.alac", ZUNE_FORMATS));
        assert!(needs_transcoding("song.aiff", ZUNE_FORMATS));
    }

    #[test]
    fn needs_transcoding_no_extension() {
        assert!(needs_transcoding("noext", ZUNE_FORMATS));
        assert!(needs_transcoding("", ZUNE_FORMATS));
    }

    // -- transcode_to_mp3 tests --

    /// Generate a minimal valid WAV file (PCM s16le, stereo, 44100 Hz).
    fn make_wav(path: &std::path::Path, num_samples: usize) {
        use std::io::Write;
        let channels: u16 = 2;
        let sample_rate: u32 = 44100;
        let bits_per_sample: u16 = 16;
        let byte_rate = sample_rate * u32::from(channels) * u32::from(bits_per_sample) / 8;
        let block_align = channels * bits_per_sample / 8;
        let data_size =
            (num_samples * usize::from(channels) * usize::from(bits_per_sample) / 8) as u32;
        let file_size = 36 + data_size;

        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(b"RIFF").unwrap();
        f.write_all(&file_size.to_le_bytes()).unwrap();
        f.write_all(b"WAVE").unwrap();
        f.write_all(b"fmt ").unwrap();
        f.write_all(&16u32.to_le_bytes()).unwrap(); // chunk size
        f.write_all(&1u16.to_le_bytes()).unwrap(); // PCM
        f.write_all(&channels.to_le_bytes()).unwrap();
        f.write_all(&sample_rate.to_le_bytes()).unwrap();
        f.write_all(&byte_rate.to_le_bytes()).unwrap();
        f.write_all(&block_align.to_le_bytes()).unwrap();
        f.write_all(&bits_per_sample.to_le_bytes()).unwrap();
        f.write_all(b"data").unwrap();
        f.write_all(&data_size.to_le_bytes()).unwrap();
        // Write silence (zeros)
        let silence = vec![0u8; data_size as usize];
        f.write_all(&silence).unwrap();
    }

    /// WAV with `audio_frames` of low-amplitude alternating samples followed
    /// by `silence_frames` of exact zeros (stereo s16le @ 44.1 kHz).
    fn make_wav_tail(path: &std::path::Path, audio_frames: usize, silence_frames: usize) {
        use std::io::Write;
        let channels: u16 = 2;
        let sample_rate: u32 = 44100;
        let bits_per_sample: u16 = 16;
        let byte_rate = sample_rate * u32::from(channels) * u32::from(bits_per_sample) / 8;
        let block_align = channels * bits_per_sample / 8;
        let total_frames = audio_frames + silence_frames;
        let data_size =
            (total_frames * usize::from(channels) * usize::from(bits_per_sample) / 8) as u32;
        let file_size = 36 + data_size;

        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(b"RIFF").unwrap();
        f.write_all(&file_size.to_le_bytes()).unwrap();
        f.write_all(b"WAVE").unwrap();
        f.write_all(b"fmt ").unwrap();
        f.write_all(&16u32.to_le_bytes()).unwrap();
        f.write_all(&1u16.to_le_bytes()).unwrap();
        f.write_all(&channels.to_le_bytes()).unwrap();
        f.write_all(&sample_rate.to_le_bytes()).unwrap();
        f.write_all(&byte_rate.to_le_bytes()).unwrap();
        f.write_all(&block_align.to_le_bytes()).unwrap();
        f.write_all(&bits_per_sample.to_le_bytes()).unwrap();
        f.write_all(b"data").unwrap();
        f.write_all(&data_size.to_le_bytes()).unwrap();

        for i in 0..audio_frames {
            let v: i16 = if i % 2 == 0 { 2000 } else { -2000 };
            let bytes = v.to_le_bytes();
            for _ in 0..channels {
                f.write_all(&bytes).unwrap();
            }
        }
        let silence = vec![0u8; silence_frames * usize::from(channels) * 2];
        f.write_all(&silence).unwrap();
    }

    fn count_mp3_frames(mp3_path: &str) -> u64 {
        use symphonia::core::codecs::DecoderOptions;
        use symphonia::core::formats::FormatOptions;
        use symphonia::core::io::MediaSourceStream;
        use symphonia::core::meta::MetadataOptions;
        use symphonia::core::probe::Hint;

        let file = std::fs::File::open(mp3_path).unwrap();
        let mss = MediaSourceStream::new(Box::new(file), Default::default());
        let mut hint = Hint::new();
        hint.with_extension("mp3");
        let probed = symphonia::default::get_probe()
            .format(
                &hint,
                mss,
                &FormatOptions::default(),
                &MetadataOptions::default(),
            )
            .unwrap();
        let mut format = probed.format;
        let track = format.default_track().unwrap().clone();
        let mut decoder = symphonia::default::get_codecs()
            .make(&track.codec_params, &DecoderOptions::default())
            .unwrap();

        let mut total: u64 = 0;
        loop {
            let packet = match format.next_packet() {
                Ok(p) => p,
                Err(symphonia::core::errors::Error::IoError(ref e))
                    if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    break;
                }
                Err(_) => break,
            };
            if packet.track_id() != track.id {
                continue;
            }
            let decoded = decoder.decode(&packet).unwrap();
            total += decoded.frames() as u64;
        }
        total
    }

    #[test]
    fn last_nonzero_frame_end_finds_tail() {
        // Stereo: [L0 R0 L1 R1 L2 R2]
        let samples = [0.1f32, 0.2, 0.3, 0.4, 0.0, 0.0];
        assert_eq!(last_nonzero_frame_end(&samples, 2), 4);

        let all_zero = [0.0f32; 8];
        assert_eq!(last_nonzero_frame_end(&all_zero, 2), 0);

        // Non-zero only in the right channel of the last frame — still a
        // frame with content, must round up to include both samples.
        let right_only = [0.0f32, 0.0, 0.0, 0.5];
        assert_eq!(last_nonzero_frame_end(&right_only, 2), 4);

        // Non-zero in one sample of the middle frame; trailing frame is zero.
        let mid_only = [0.0f32, 0.0, 0.1, 0.0, 0.0, 0.0];
        assert_eq!(last_nonzero_frame_end(&mid_only, 2), 4);

        // Mono
        let mono = [0.0f32, 0.5, 0.0];
        assert_eq!(last_nonzero_frame_end(&mono, 1), 2);
    }

    #[test]
    fn transcode_trims_trailing_silence() {
        let dir = std::env::temp_dir().join("zytunes-test-transcode-trim");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let wav_path = dir.join("tail.wav");
        // 1s of audio + 7s of digital silence (the slowerpace pattern).
        make_wav_tail(&wav_path, 44_100, 44_100 * 7);

        let out_dir = dir.join("out");
        std::fs::create_dir_all(&out_dir).unwrap();

        let mp3_path =
            transcode_to_mp3(wav_path.to_str().unwrap(), &out_dir, Some((200, 200))).unwrap();

        let frames = count_mp3_frames(&mp3_path);

        // Without the trim, output would be ~8s (~352,800 frames). With the
        // trim it should be ~1s plus a small LAME delay — comfortably under
        // 2s. Guard against a regression where the 7s tail leaks back in.
        assert!(
            frames < 44_100 * 2,
            "output still contains trailing silence: {} frames (~{:.2}s)",
            frames,
            frames as f64 / 44_100.0
        );
        // Sanity check: we didn't strip the actual audio.
        assert!(
            frames > 22_050,
            "output suspiciously short: {} frames (~{:.3}s)",
            frames,
            frames as f64 / 44_100.0
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn transcode_preserves_mid_track_silence() {
        // A gap of zeros between two regions of audio should survive.
        let dir = std::env::temp_dir().join("zytunes-test-transcode-gap");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let wav_path = dir.join("gap.wav");
        // Build: 1s audio, 2s silence, 1s audio. We write it manually so the
        // trailing audio forces the held-zero buffer to flush.
        {
            use std::io::Write;
            let channels: u16 = 2;
            let sample_rate: u32 = 44100;
            let bits_per_sample: u16 = 16;
            let audio_frames = 44_100usize;
            let silence_frames = 44_100usize * 2;
            let total_frames = audio_frames * 2 + silence_frames;
            let byte_rate = sample_rate * u32::from(channels) * u32::from(bits_per_sample) / 8;
            let block_align = channels * bits_per_sample / 8;
            let data_size =
                (total_frames * usize::from(channels) * usize::from(bits_per_sample) / 8) as u32;
            let file_size = 36 + data_size;

            let mut f = std::fs::File::create(&wav_path).unwrap();
            f.write_all(b"RIFF").unwrap();
            f.write_all(&file_size.to_le_bytes()).unwrap();
            f.write_all(b"WAVE").unwrap();
            f.write_all(b"fmt ").unwrap();
            f.write_all(&16u32.to_le_bytes()).unwrap();
            f.write_all(&1u16.to_le_bytes()).unwrap();
            f.write_all(&channels.to_le_bytes()).unwrap();
            f.write_all(&sample_rate.to_le_bytes()).unwrap();
            f.write_all(&byte_rate.to_le_bytes()).unwrap();
            f.write_all(&block_align.to_le_bytes()).unwrap();
            f.write_all(&bits_per_sample.to_le_bytes()).unwrap();
            f.write_all(b"data").unwrap();
            f.write_all(&data_size.to_le_bytes()).unwrap();

            for i in 0..audio_frames {
                let v: i16 = if i % 2 == 0 { 2000 } else { -2000 };
                let bytes = v.to_le_bytes();
                for _ in 0..channels {
                    f.write_all(&bytes).unwrap();
                }
            }
            let silence = vec![0u8; silence_frames * usize::from(channels) * 2];
            f.write_all(&silence).unwrap();
            for i in 0..audio_frames {
                let v: i16 = if i % 2 == 0 { 2000 } else { -2000 };
                let bytes = v.to_le_bytes();
                for _ in 0..channels {
                    f.write_all(&bytes).unwrap();
                }
            }
        }

        let out_dir = dir.join("out");
        std::fs::create_dir_all(&out_dir).unwrap();

        let mp3_path =
            transcode_to_mp3(wav_path.to_str().unwrap(), &out_dir, Some((200, 200))).unwrap();

        let frames = count_mp3_frames(&mp3_path);
        // Total source = 4s. Output should keep the middle gap (mid-track
        // silence is not trailing), so frames ~= 4s worth. Allow slack for
        // LAME delay/padding on either side.
        assert!(
            frames > 44_100 * 3,
            "mid-track silence was incorrectly trimmed: {} frames (~{:.2}s)",
            frames,
            frames as f64 / 44_100.0
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Write a 44.1 kHz / 16-bit sine-tone WAV with the given channel count.
    /// Unlike `make_wav` (silence) this produces real signal so duration
    /// assertions can't be confused with the silence-trimming logic.
    fn make_sine_wav(path: &std::path::Path, seconds: u32, channels: u16) {
        use std::io::Write;
        let sample_rate: u32 = 44_100;
        let bits: u16 = 16;
        let frames = sample_rate * seconds;
        let data_size = frames * u32::from(channels) * u32::from(bits) / 8;
        let byte_rate = sample_rate * u32::from(channels) * u32::from(bits) / 8;
        let block_align = channels * bits / 8;

        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(b"RIFF").unwrap();
        f.write_all(&(36 + data_size).to_le_bytes()).unwrap();
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
            let t = frame as f32 / sample_rate as f32;
            let v = ((t * 440.0 * std::f32::consts::TAU).sin() * 20_000.0) as i16;
            for _ in 0..channels {
                pcm.extend_from_slice(&v.to_le_bytes());
            }
        }
        f.write_all(&pcm).unwrap();
    }

    /// Transcoded output must be roughly as long as the source — not doubled
    /// (audio repeated) and not halved (frame-count confusion). Regression
    /// guard for duration bugs in the decode→LAME loop.
    fn assert_transcode_duration(channels: u16, label: &str) {
        let dir = std::env::temp_dir().join(format!("zytunes-test-dur-{label}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("tone.wav");
        make_sine_wav(&wav, 3, channels);

        let mp3 = transcode_to_mp3(wav.to_str().unwrap(), &dir, None).unwrap();
        let frames = count_mp3_frames(&mp3);
        let secs = frames as f64 / 44_100.0;
        assert!(
            (2.5..=4.0).contains(&secs),
            "{label}: 3s source transcoded to {secs:.2}s ({frames} frames)"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn transcode_preserves_duration_stereo() {
        assert_transcode_duration(2, "stereo");
    }

    #[test]
    fn transcode_preserves_duration_mono() {
        assert_transcode_duration(1, "mono");
    }

    /// Locate the first MPEG frame in an MP3 file (skipping any ID3v2 tag)
    /// Offset of the Xing tag inside the first MPEG frame at `off` —
    /// 4-byte header + version/channel-dependent side info, derived from
    /// the actual frame header the same way `patch_xing_header` does.
    fn xing_tag_start(data: &[u8], off: usize) -> usize {
        let hdr = parse_mp3_frame_header(&data[off..]).expect("first MPEG frame header");
        let side_info = match (hdr.mpeg1, hdr.mono) {
            (true, false) => 32,
            (true, true) => 17,
            (false, false) => 17,
            (false, true) => 9,
        };
        off + 4 + side_info
    }

    /// and return (stream offset, stream bytes).
    fn mp3_stream_bounds(data: &[u8]) -> (usize, usize) {
        let mut off = 0usize;
        if data.starts_with(b"ID3") && data.len() > 10 {
            let sz = ((data[6] as usize) << 21)
                | ((data[7] as usize) << 14)
                | ((data[8] as usize) << 7)
                | (data[9] as usize);
            off = 10 + sz;
        }
        (off, data.len() - off)
    }

    /// The transcoder must fill in LAME's reserved Xing placeholder.
    /// Without it, players estimate duration from the placeholder's nominal
    /// bitrate — roughly 2× real length for typical VBR music — which is how
    /// tracks synced to the Zune ended up "twice as long, played twice".
    #[test]
    fn transcoded_mp3_has_valid_xing_header() {
        let dir = std::env::temp_dir().join("zytunes-test-xing");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("tone.wav");
        make_sine_wav(&wav, 3, 2);

        let mp3 = transcode_to_mp3(wav.to_str().unwrap(), &dir, None).unwrap();
        let data = std::fs::read(&mp3).unwrap();
        let (off, stream_len) = mp3_stream_bounds(&data);

        let tag_start = xing_tag_start(&data, off);
        assert_eq!(
            &data[tag_start..tag_start + 4],
            b"Xing",
            "no Xing magic at expected offset — placeholder left unpatched"
        );
        let be_u32 =
            |i: usize| u32::from_be_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]);
        let flags = be_u32(tag_start + 4);
        assert_eq!(flags & 0x3, 0x3, "FRAMES and BYTES flags must be set");
        let frames = be_u32(tag_start + 8);
        let bytes = be_u32(tag_start + 12);

        // Duration players derive from the tag must match the real 3 s source.
        let tag_secs = frames as f64 * 1152.0 / 44_100.0;
        assert!(
            (2.5..=4.0).contains(&tag_secs),
            "Xing frame count implies {tag_secs:.2}s for a 3s source"
        );
        assert_eq!(
            bytes as usize, stream_len,
            "Xing byte count must equal the MPEG stream length"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Duration-by-Xing must agree with duration-by-decode — the exact
    /// mismatch that produced doubled-length tracks on device.
    #[test]
    fn xing_duration_matches_decoded_duration() {
        let dir = std::env::temp_dir().join("zytunes-test-xing-dur");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("tone.wav");
        make_sine_wav(&wav, 3, 2);

        let mp3 = transcode_to_mp3(wav.to_str().unwrap(), &dir, None).unwrap();
        let decoded_secs = count_mp3_frames(&mp3) as f64 / 44_100.0;

        let data = std::fs::read(&mp3).unwrap();
        let (off, _) = mp3_stream_bounds(&data);
        let tag_start = xing_tag_start(&data, off);
        let frames = u32::from_be_bytes([
            data[tag_start + 8],
            data[tag_start + 9],
            data[tag_start + 10],
            data[tag_start + 11],
        ]);
        let tag_secs = frames as f64 * 1152.0 / 44_100.0;
        assert!(
            (tag_secs - decoded_secs).abs() < 0.25,
            "Xing duration {tag_secs:.2}s vs decoded {decoded_secs:.2}s"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Generate a minimal JPEG image of given dimensions.
    fn make_jpeg(width: u32, height: u32) -> Vec<u8> {
        use image::{ImageBuffer, Rgb};
        let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::new(width, height);
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Jpeg).unwrap();
        buf.into_inner()
    }

    #[test]
    fn transcode_wav_to_mp3() {
        let dir = std::env::temp_dir().join("zytunes-test-transcode-wav");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let wav_path = dir.join("silence.wav");
        make_wav(&wav_path, 44100); // 1 second of stereo silence

        let out_dir = dir.join("out");
        std::fs::create_dir_all(&out_dir).unwrap();

        let result = transcode_to_mp3(wav_path.to_str().unwrap(), &out_dir, Some((200, 200)));
        assert!(result.is_ok(), "transcode failed: {:?}", result.err());

        let mp3_path = result.unwrap();
        assert!(mp3_path.ends_with(".mp3"));
        assert!(std::path::Path::new(&mp3_path).exists());

        // Verify it's a valid MP3 (starts with ID3 header or MPEG sync)
        let data = std::fs::read(&mp3_path).unwrap();
        assert!(!data.is_empty());
        let has_id3 = data.starts_with(b"ID3");
        let has_sync = data.len() >= 2 && data[0] == 0xff && (data[1] & 0xe0) == 0xe0;
        assert!(has_id3 || has_sync, "output is not a valid MP3 file");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn transcode_preserves_metadata() {
        let dir = std::env::temp_dir().join("zytunes-test-transcode-meta");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let wav_path = dir.join("tagged.wav");
        make_wav(&wav_path, 44100);

        // Write metadata to the WAV using lofty
        {
            use lofty::file::TaggedFileExt;
            use lofty::tag::{Accessor, Tag, TagExt};
            let mut tagged = lofty::probe::read_from_path(&wav_path).unwrap();
            let tag_type = tagged.primary_tag_type();
            // Insert a tag if none exists (WAV files may not have one)
            if tagged.primary_tag().is_none() {
                tagged.insert_tag(Tag::new(tag_type));
            }
            let tag = tagged.primary_tag_mut().unwrap();
            tag.set_artist("Test Artist".to_string());
            tag.set_album("Test Album".to_string());
            tag.set_title("Test Title".to_string());
            tag.set_track(7);
            tag.set_genre("Rock".to_string());
            tag.save_to_path(&wav_path, lofty::config::WriteOptions::default())
                .unwrap();
        }

        let out_dir = dir.join("out");
        std::fs::create_dir_all(&out_dir).unwrap();

        let mp3_path =
            transcode_to_mp3(wav_path.to_str().unwrap(), &out_dir, Some((200, 200))).unwrap();

        // Read back the ID3 tags from the output MP3
        let tag = id3::Tag::read_from_path(&mp3_path).unwrap();
        assert_eq!(tag.artist(), Some("Test Artist"));
        assert_eq!(tag.album(), Some("Test Album"));
        assert_eq!(tag.title(), Some("Test Title"));
        assert_eq!(tag.track(), Some(7));
        assert_eq!(tag.genre_parsed().as_deref(), Some("Rock"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn transcode_resizes_album_art() {
        let dir = std::env::temp_dir().join("zytunes-test-transcode-art");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let wav_path = dir.join("art.wav");
        make_wav(&wav_path, 44100);

        // Embed a 500x500 JPEG as album art
        let big_art = make_jpeg(500, 500);
        {
            use lofty::file::TaggedFileExt;
            use lofty::picture::{MimeType, Picture, PictureType};
            use lofty::tag::{Accessor, Tag, TagExt};
            let mut tagged = lofty::probe::read_from_path(&wav_path).unwrap();
            let tag_type = tagged.primary_tag_type();
            if tagged.primary_tag().is_none() {
                tagged.insert_tag(Tag::new(tag_type));
            }
            let tag = tagged.primary_tag_mut().unwrap();
            tag.set_artist("Art Artist".to_string());
            let pic = Picture::new_unchecked(
                PictureType::CoverFront,
                Some(MimeType::Jpeg),
                None,
                big_art,
            );
            tag.push_picture(pic);
            tag.save_to_path(&wav_path, lofty::config::WriteOptions::default())
                .unwrap();
        }

        let out_dir = dir.join("out");
        std::fs::create_dir_all(&out_dir).unwrap();

        let mp3_path =
            transcode_to_mp3(wav_path.to_str().unwrap(), &out_dir, Some((200, 200))).unwrap();

        // Read the embedded art from the output MP3 and check dimensions
        let tag = id3::Tag::read_from_path(&mp3_path).unwrap();
        let pics: Vec<_> = tag.pictures().collect();
        assert!(!pics.is_empty(), "no album art in output MP3");
        let art_data = &pics[0].data;
        let img = image::load_from_memory(art_data).unwrap();
        assert_eq!(img.width(), 200);
        assert_eq!(img.height(), 200);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn transcode_no_art_still_works() {
        let dir = std::env::temp_dir().join("zytunes-test-transcode-noart");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let wav_path = dir.join("noart.wav");
        make_wav(&wav_path, 44100);

        let out_dir = dir.join("out");
        std::fs::create_dir_all(&out_dir).unwrap();

        let result = transcode_to_mp3(wav_path.to_str().unwrap(), &out_dir, Some((200, 200)));
        assert!(result.is_ok(), "transcode failed: {:?}", result.err());

        // Verify no art embedded
        let mp3_path = result.unwrap();
        let tag = id3::Tag::read_from_path(&mp3_path);
        if let Ok(tag) = tag {
            assert_eq!(tag.pictures().count(), 0);
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn transcode_nonexistent_file_errors() {
        let out_dir = std::env::temp_dir().join("zytunes-test-transcode-nofile");
        let _ = std::fs::remove_dir_all(&out_dir);
        std::fs::create_dir_all(&out_dir).unwrap();

        let result = transcode_to_mp3("/nonexistent/path/song.flac", &out_dir, Some((200, 200)));
        assert!(result.is_err());

        let _ = std::fs::remove_dir_all(&out_dir);
    }
}
