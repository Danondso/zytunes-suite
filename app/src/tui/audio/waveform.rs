//! Live spectrum tap for the now-playing soundbar.
//!
//! The audio thread wraps every appended source in [`WaveformTap`], which
//! FFTs the current mix and publishes 64 log-frequency bars. The UI reads
//! that snapshot each frame — columns are bass→treble and stay put, no
//! extra `AudioEvent`s, no locks on the mix callback.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

use rodio::{ChannelCount, Sample, SampleRate, Source};

/// Columns in the published snapshot (bass on the left, treble on the right).
pub const BINS: usize = 64;

const FFT_N: usize = 1024;
/// Per-frame release so bars fall over a few TUI ticks instead of blinking.
const FALL: f32 = 0.78;

/// Shared snapshot of the current spectrum. Audio thread publishes a full
/// frame at a time; the UI reads it in place.
pub struct Waveform {
    bins: [AtomicU8; BINS],
}

impl Waveform {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            bins: std::array::from_fn(|_| AtomicU8::new(0)),
        })
    }

    pub fn clear(&self) {
        for b in &self.bins {
            b.store(0, Ordering::Relaxed);
        }
    }

    fn publish(&self, frame: &[u8; BINS]) {
        for (bin, v) in self.bins.iter().zip(frame) {
            bin.store(*v, Ordering::Relaxed);
        }
    }

    /// Left → right of the current snapshot, resampled to `n` columns.
    /// Empty `n` yields empty. Wider than [`BINS`] repeats neighbouring
    /// peaks; narrower takes the max in each source group so transients
    /// are not averaged away.
    pub fn columns(&self, n: usize) -> Vec<u8> {
        if n == 0 {
            return Vec::new();
        }
        let mut out = vec![0u8; n];
        for (c, slot) in out.iter_mut().enumerate() {
            let start = c * BINS / n;
            let end = ((c + 1) * BINS / n).max(start + 1).min(BINS);
            let mut m = 0u8;
            for i in start..end {
                let v = self.bins[i].load(Ordering::Relaxed);
                if v > m {
                    m = v;
                }
            }
            *slot = m;
        }
        out
    }
}

/// Pass-through [`Source`] that records spectrum amplitude into `wave`.
pub struct WaveformTap<S: Source> {
    inner: S,
    wave: Arc<Waveform>,
    channels: u16,
    channel_pos: u16,
    frame_acc: f32,
    time: [f32; FFT_N],
    time_at: usize,
    bands: [(usize, usize); BINS],
    shown: [f32; BINS],
}

impl<S: Source> WaveformTap<S> {
    pub fn new(inner: S, wave: Arc<Waveform>) -> Self {
        let sr = inner.sample_rate().get().max(1);
        let channels = inner.channels().get().max(1);
        Self {
            inner,
            wave,
            channels,
            channel_pos: 0,
            frame_acc: 0.0,
            time: [0.0; FFT_N],
            time_at: 0,
            bands: band_ranges(sr),
            shown: [0.0; BINS],
        }
    }

    fn ingest(&mut self, sample: Sample) {
        self.frame_acc += sample;
        self.channel_pos += 1;
        if self.channel_pos < self.channels {
            return;
        }
        let mono = self.frame_acc / f32::from(self.channels);
        self.frame_acc = 0.0;
        self.channel_pos = 0;
        self.time[self.time_at] = mono;
        self.time_at += 1;
        if self.time_at == FFT_N {
            self.analyze();
            self.time_at = 0;
        }
    }

    fn analyze(&mut self) {
        let mut re = [0.0f32; FFT_N];
        let mut im = [0.0f32; FFT_N];
        let n = FFT_N as f32;
        for (i, (r, sample)) in re.iter_mut().zip(self.time.iter()).enumerate() {
            let hann = 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / n).cos();
            *r = sample * hann;
        }
        fft_radix2(&mut re, &mut im);

        // Hann coherent gain 0.5 → *2, then 2/N for a real-FFT one-sided bin.
        let scale = 4.0 / n;
        let mut frame = [0u8; BINS];
        for (c, slot) in frame.iter_mut().enumerate() {
            let (lo, hi) = self.bands[c];
            let mut mag = 0.0f32;
            for k in lo..hi {
                mag = mag.max((re[k] * re[k] + im[k] * im[k]).sqrt() * scale);
            }
            let v = mag.min(1.0);
            self.shown[c] = if v >= self.shown[c] {
                v
            } else {
                self.shown[c] * FALL
            };
            *slot = (self.shown[c] * 255.0) as u8;
        }
        self.wave.publish(&frame);
    }
}

/// Log-spaced FFT-bin ranges from ~40 Hz to 16 kHz (or Nyquist).
fn band_ranges(sr: u32) -> [(usize, usize); BINS] {
    let sr = sr.max(1) as f32;
    let nyquist = sr / 2.0;
    let bin_hz = sr / FFT_N as f32;
    let f_min = 40.0_f32.min(nyquist / 4.0).max(bin_hz);
    let f_max = nyquist.min(16_000.0).max(f_min * 2.0);
    let ratio = f_max / f_min;
    let mut out = [(1usize, 2usize); BINS];
    for (c, slot) in out.iter_mut().enumerate() {
        let lo = f_min * ratio.powf(c as f32 / BINS as f32);
        let hi = f_min * ratio.powf((c + 1) as f32 / BINS as f32);
        let i0 = ((lo / bin_hz) as usize).clamp(1, FFT_N / 2 - 1);
        let i1 = ((hi / bin_hz) as usize).clamp(i0 + 1, FFT_N / 2);
        *slot = (i0, i1);
    }
    out
}

fn fft_radix2(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    debug_assert_eq!(n, im.len());
    debug_assert!(n.is_power_of_two());

    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j ^= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }

    let mut len = 2;
    while len <= n {
        let ang = -std::f32::consts::TAU / len as f32;
        let (wlen_re, wlen_im) = (ang.cos(), ang.sin());
        for i in (0..n).step_by(len) {
            let mut w_re = 1.0f32;
            let mut w_im = 0.0f32;
            for j in 0..len / 2 {
                let u_re = re[i + j];
                let u_im = im[i + j];
                let vr = re[i + j + len / 2];
                let vi = im[i + j + len / 2];
                let v_re = vr * w_re - vi * w_im;
                let v_im = vr * w_im + vi * w_re;
                re[i + j] = u_re + v_re;
                im[i + j] = u_im + v_im;
                re[i + j + len / 2] = u_re - v_re;
                im[i + j + len / 2] = u_im - v_im;
                let nw_re = w_re * wlen_re - w_im * wlen_im;
                w_im = w_re * wlen_im + w_im * wlen_re;
                w_re = nw_re;
            }
        }
        len *= 2;
    }
}

impl<S: Source> Iterator for WaveformTap<S> {
    type Item = Sample;

    fn next(&mut self) -> Option<Sample> {
        let s = self.inner.next()?;
        self.ingest(s);
        Some(s)
    }
}

impl<S: Source> Source for WaveformTap<S> {
    fn current_span_len(&self) -> Option<usize> {
        self.inner.current_span_len()
    }

    fn channels(&self) -> ChannelCount {
        self.inner.channels()
    }

    fn sample_rate(&self) -> SampleRate {
        self.inner.sample_rate()
    }

    fn total_duration(&self) -> Option<Duration> {
        self.inner.total_duration()
    }

    fn try_seek(&mut self, pos: Duration) -> Result<(), rodio::source::SeekError> {
        self.inner.try_seek(pos)?;
        self.channel_pos = 0;
        self.frame_acc = 0.0;
        self.time_at = 0;
        self.shown = [0.0; BINS];
        Ok(())
    }
}

/// Map a 0–255 amplitude onto `levels` (sparse → dense).
///
/// `sqrt` lifts typical mix levels out of the floor glyphs so the bar
/// reads as a contour instead of a thin spike train.
pub fn glyph_from_levels(peak: u8, levels: &[char]) -> char {
    if levels.is_empty() {
        return ' ';
    }
    let boosted = ((peak as f32 / 255.0).sqrt() * 255.0) as usize;
    let i = (boosted * (levels.len() - 1) + 127) / 255;
    levels[i.min(levels.len() - 1)]
}

#[cfg(test)]
pub fn bar_glyph(peak: u8) -> char {
    glyph_from_levels(peak, RAMP_BLOCKS)
}

/// Block heights — iTunes / Gruvbox / Zune.
pub const RAMP_BLOCKS: &[char] = &[' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
/// Bubble dots — Amber CRT / Newport.
pub const RAMP_DOTS: &[char] = &[' ', '·', '•', '●'];
/// Shade ramp — Windows 95 / System 7.
pub const RAMP_SHADE: &[char] = &[' ', '░', '▒', '▓', '█'];
/// ASCII density — BIOS.
pub const RAMP_ASCII: &[char] = &[' ', '.', ':', '-', '=', '+', '*', '#', '@'];
/// Braille fill — Tokyo Night.
pub const RAMP_BRAILLE: &[char] = &[' ', '⡀', '⣀', '⣠', '⣤', '⣦', '⣶', '⣷', '⣿'];
/// Three-level chunky — IBM / NeXTSTEP.
pub const RAMP_CHUNKY: &[char] = &[' ', '▄', '█'];

/// In-place soundbar draw styles. `W` cycles these; all of them only
/// change column height, never slide sideways.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SoundbarStyle {
    /// A dozen double-wide EQ meters (default).
    #[default]
    Meters,
    /// Thinner graphic-EQ, more bands.
    Eq,
    /// Bass on both edges, treble meeting in the middle.
    Mirror,
    /// One energy level shaped into a centered mountain.
    Pulse,
    /// Same layout as [`Self::Meters`] with a dot ramp.
    Dots,
}

impl SoundbarStyle {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Meters => "meters",
            Self::Eq => "eq",
            Self::Mirror => "mirror",
            Self::Pulse => "pulse",
            Self::Dots => "dots",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Meters => "Soundbar: meters",
            Self::Eq => "Soundbar: eq",
            Self::Mirror => "Soundbar: mirror",
            Self::Pulse => "Soundbar: pulse",
            Self::Dots => "Soundbar: dots",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Meters => Self::Eq,
            Self::Eq => Self::Mirror,
            Self::Mirror => Self::Pulse,
            Self::Pulse => Self::Dots,
            Self::Dots => Self::Meters,
        }
    }
}

impl std::str::FromStr for SoundbarStyle {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "meters" => Ok(Self::Meters),
            "eq" => Ok(Self::Eq),
            "mirror" => Ok(Self::Mirror),
            "pulse" => Ok(Self::Pulse),
            "dots" => Ok(Self::Dots),
            _ => Err(()),
        }
    }
}

/// One cell of a rendered soundbar. Gaps are the spaces between meters.
pub struct SoundbarAtom {
    pub ch: char,
    pub peak: u8,
    /// 0 = leftmost (bass), 1 = rightmost (treble).
    pub pos: f32,
    pub gap: bool,
}

/// Draw one centered-ready soundbar. `ramp` is the theme's glyph alphabet;
/// [`SoundbarStyle::Dots`] forces [`RAMP_DOTS`].
#[cfg(test)]
pub fn render_soundbar(
    wave: &Waveform,
    style: SoundbarStyle,
    width: usize,
    ramp: &[char],
) -> String {
    soundbar_plain(&soundbar_atoms(wave, style, width, ramp))
}

pub fn soundbar_atoms(
    wave: &Waveform,
    style: SoundbarStyle,
    width: usize,
    ramp: &[char],
) -> Vec<SoundbarAtom> {
    if width == 0 {
        return Vec::new();
    }
    let ramp = if matches!(style, SoundbarStyle::Dots) {
        RAMP_DOTS
    } else {
        ramp
    };
    match style {
        SoundbarStyle::Meters | SoundbarStyle::Dots => {
            let n = ((width + 1) / 3).clamp(1, 12);
            paint(&wave.columns(n), ramp, true)
        }
        SoundbarStyle::Eq => {
            let n = width.div_ceil(2).clamp(1, 24);
            paint(&wave.columns(n), ramp, false)
        }
        SoundbarStyle::Mirror => {
            let n = (((width + 1) / 3).clamp(2, 16) / 2) * 2;
            let n = n.max(2);
            let half = wave.columns(n / 2);
            let peaks: Vec<u8> = half
                .iter()
                .copied()
                .chain(half.iter().rev().copied())
                .collect();
            paint(&peaks, ramp, true)
        }
        SoundbarStyle::Pulse => {
            let n = ((width + 1) / 3).clamp(1, 12);
            let level = f32::from(wave.columns(1).first().copied().unwrap_or(0));
            let peaks: Vec<u8> = (0..n)
                .map(|i| {
                    let x = if n == 1 {
                        0.0
                    } else {
                        (i as f32 / (n - 1) as f32) * 2.0 - 1.0
                    };
                    (level * (1.0 - x * x).max(0.0)) as u8
                })
                .collect();
            paint(&peaks, ramp, true)
        }
    }
}

#[cfg(test)]
fn soundbar_plain(atoms: &[SoundbarAtom]) -> String {
    atoms.iter().map(|a| a.ch).collect()
}

fn paint(peaks: &[u8], ramp: &[char], double: bool) -> Vec<SoundbarAtom> {
    let n = peaks.len();
    let mut out = Vec::new();
    for (i, p) in peaks.iter().enumerate() {
        if i > 0 {
            out.push(SoundbarAtom {
                ch: ' ',
                peak: 0,
                pos: 0.0,
                gap: true,
            });
        }
        let pos = if n <= 1 {
            0.5
        } else {
            i as f32 / (n - 1) as f32
        };
        let g = glyph_from_levels(*p, ramp);
        out.push(SoundbarAtom {
            ch: g,
            peak: *p,
            pos,
            gap: false,
        });
        if double {
            out.push(SoundbarAtom {
                ch: g,
                peak: *p,
                pos,
                gap: false,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use rodio::buffer::SamplesBuffer;
    use std::num::NonZero;

    fn mono(sr: u32, data: Vec<f32>) -> SamplesBuffer {
        SamplesBuffer::new(NonZero::new(1).unwrap(), NonZero::new(sr).unwrap(), data)
    }

    fn argmax(cols: &[u8]) -> usize {
        cols.iter()
            .enumerate()
            .max_by_key(|(_, v)| *v)
            .map(|(i, _)| i)
            .unwrap()
    }

    #[test]
    fn tap_is_passthrough() {
        let wave = Waveform::new();
        let src = mono(44_100, vec![0.1, 0.2, 0.3]);
        let out: Vec<f32> = WaveformTap::new(src, wave).collect();
        assert_eq!(out, vec![0.1, 0.2, 0.3]);
    }

    #[test]
    fn fft_peaks_at_integer_bin() {
        let mut re: Vec<f32> = (0..FFT_N)
            .map(|i| (std::f32::consts::TAU * 8.0 * i as f32 / FFT_N as f32).sin())
            .collect();
        let mut im = vec![0.0f32; FFT_N];
        fft_radix2(&mut re, &mut im);
        let peak = (0..FFT_N / 2)
            .max_by(|a, b| {
                let pa = re[*a] * re[*a] + im[*a] * im[*a];
                let pb = re[*b] * re[*b] + im[*b] * im[*b];
                pa.partial_cmp(&pb).unwrap()
            })
            .unwrap();
        assert_eq!(peak, 8);
    }

    #[test]
    fn sine_stays_in_the_same_column() {
        let sr = 44_100u32;
        let freq = 440.0f32;
        let data: Vec<f32> = (0..FFT_N * 2)
            .map(|i| (std::f32::consts::TAU * freq * i as f32 / sr as f32).sin())
            .collect();
        let wave = Waveform::new();
        let mut tap = WaveformTap::new(mono(sr, data), Arc::clone(&wave));
        for _ in 0..FFT_N {
            tap.next();
        }
        let first = argmax(&wave.columns(BINS));
        for _ in 0..FFT_N {
            tap.next();
        }
        let second = argmax(&wave.columns(BINS));
        assert_eq!(first, second, "spectrum columns must not walk with time");
        assert!(
            first > 0 && first < BINS - 1,
            "440 Hz should not sit at an edge"
        );
    }

    #[test]
    fn columns_downsample_keeps_max() {
        let wave = Waveform::new();
        let mut frame = [0u8; BINS];
        frame[0] = 10;
        frame[1] = 200;
        frame[2] = 10;
        frame[3] = 10;
        wave.publish(&frame);
        let cols = wave.columns(1);
        assert_eq!(cols, vec![200], "single column must keep the transient");
    }

    #[test]
    fn clear_zeros_the_snapshot() {
        let wave = Waveform::new();
        wave.publish(&[255; BINS]);
        wave.clear();
        assert!(wave.columns(8).iter().all(|&v| v == 0));
    }

    #[test]
    fn bar_glyph_ends() {
        assert_eq!(bar_glyph(0), ' ');
        assert_eq!(bar_glyph(255), '█');
    }

    #[test]
    fn theme_ramps_use_their_own_alphabet() {
        assert_eq!(glyph_from_levels(255, RAMP_ASCII), '@');
        assert_eq!(glyph_from_levels(255, RAMP_BRAILLE), '⣿');
        assert_eq!(glyph_from_levels(255, RAMP_SHADE), '█');
        assert_eq!(glyph_from_levels(0, RAMP_CHUNKY), ' ');
    }

    #[test]
    fn soundbar_style_round_trip_and_cycle() {
        let mut style = SoundbarStyle::Meters;
        let mut seen = std::collections::HashSet::new();
        for _ in 0..5 {
            assert!(seen.insert(style.as_str()));
            assert_eq!(style.as_str().parse::<SoundbarStyle>().ok(), Some(style));
            style = style.next();
        }
        assert_eq!(style, SoundbarStyle::Meters);
        assert!("bogus".parse::<SoundbarStyle>().is_err());
    }

    #[test]
    fn render_soundbar_empty_width() {
        let wave = Waveform::new();
        assert!(render_soundbar(&wave, SoundbarStyle::Meters, 0, RAMP_BLOCKS).is_empty());
    }

    #[test]
    fn render_mirror_is_symmetric() {
        let wave = Waveform::new();
        let mut frame = [0u8; BINS];
        frame[0] = 255;
        wave.publish(&frame);
        let line = render_soundbar(&wave, SoundbarStyle::Mirror, 40, RAMP_BLOCKS);
        let chars: Vec<char> = line.chars().filter(|c| *c != ' ').collect();
        let mut rev = chars.clone();
        rev.reverse();
        assert_eq!(chars, rev, "mirror must read the same backwards");
    }

    #[test]
    fn dots_layout_ignores_passed_ramp() {
        let wave = Waveform::new();
        wave.publish(&[255; BINS]);
        let line = render_soundbar(&wave, SoundbarStyle::Dots, 20, RAMP_ASCII);
        assert!(
            line.chars().any(|c| c == '●' || c == '•' || c == '·'),
            "Dots must keep its own alphabet, got {line:?}"
        );
        assert!(!line.contains('@'), "Dots must not use the ASCII ramp");
    }

    #[test]
    fn render_pulse_peaks_at_center() {
        let wave = Waveform::new();
        wave.publish(&[255; BINS]);
        let line = render_soundbar(&wave, SoundbarStyle::Pulse, 40, RAMP_BLOCKS);
        let chars: Vec<char> = line.chars().filter(|c| *c != ' ').collect();
        assert!(!chars.is_empty());
        let mid = chars.len() / 2;
        assert_eq!(chars[mid], '█');
        assert_ne!(chars[0], chars[mid], "edges must sit below the centre");
    }
}
