//! Six-stem mixing source for rodio.
//!
//! One mixed `Source`, not six sinks: parallel players can drift on
//! pause/scrub and make end-of-track detection ambiguous, while a single
//! source keeps the stems sample-locked through the existing `Player`
//! machinery (`p.empty()` still drives `TrackEnded`, `skip_duration`
//! still implements scrub).
//!
//! Per-stem gains live in a shared [`StemGains`] (`f32` bits in
//! `AtomicU32`) owned by the app; toggling a stem is a lock-free store
//! with no channel round-trip. The mixer chases each target through a
//! short linear ramp so mutes/unmutes are click-free.

use std::sync::atomic::Ordering;
use std::time::Duration;

use rodio::{ChannelCount, Sample, SampleRate, Source};
use zytunes::stems::{StemGains, NUM_STEMS};

/// Seconds a gain change takes to complete — long enough to avoid an
/// audible click, short enough to feel instant.
pub const RAMP_SECONDS: f32 = 0.010;

/// Mixes [`NUM_STEMS`] equally-parameterised sources into one, applying
/// a ramped per-stem gain read from `gains` at every frame boundary.
///
/// Stems of unequal length pad with silence: the mix ends when the
/// longest source ends, so a short stem never truncates the track.
pub struct StemMixerSource<S: Source> {
    sources: [S; NUM_STEMS],
    done: [bool; NUM_STEMS],
    gains: StemGains,
    /// Smoothed per-stem gain currently applied.
    ramp: [f32; NUM_STEMS],
    /// Per-frame ramp increment: full 0→1 transition in [`RAMP_SECONDS`].
    ramp_step: f32,
    channels: ChannelCount,
    sample_rate: SampleRate,
    total: Option<Duration>,
    /// Interleaved-channel cursor; ramps advance once per frame (when it
    /// wraps to 0) so all channels of a frame share one gain value.
    channel_pos: u16,
}

impl<S: Source> StemMixerSource<S> {
    /// Build a mixer over six stems. All sources must agree on channel
    /// count and sample rate (demucs output does; anything else is a
    /// corrupt cache entry and errors here rather than playing garbage).
    pub fn new(sources: [S; NUM_STEMS], gains: StemGains) -> Result<Self, String> {
        let channels = sources[0].channels();
        let sample_rate = sources[0].sample_rate();
        for (i, s) in sources.iter().enumerate() {
            if s.channels() != channels || s.sample_rate() != sample_rate {
                return Err(format!(
                    "stem {i} has {} ch @ {} Hz but stem 0 has {} ch @ {} Hz — corrupt stem set",
                    s.channels(),
                    s.sample_rate(),
                    channels,
                    sample_rate
                ));
            }
        }
        // The longest stem defines the mix's duration; unknown (None) from
        // any source makes the total unknown.
        let total = sources
            .iter()
            .map(|s| s.total_duration())
            .try_fold(Duration::ZERO, |acc, d| d.map(|d| acc.max(d)));
        let ramp = std::array::from_fn(|i| f32::from_bits(gains[i].load(Ordering::Relaxed)));
        Ok(StemMixerSource {
            sources,
            done: [false; NUM_STEMS],
            gains,
            ramp,
            ramp_step: 1.0 / (RAMP_SECONDS * sample_rate.get() as f32),
            channels,
            sample_rate,
            total,
            channel_pos: 0,
        })
    }
}

impl<S: Source> Iterator for StemMixerSource<S> {
    type Item = Sample;

    fn next(&mut self) -> Option<Sample> {
        // Chase the gain targets once per frame so every channel of a
        // frame gets the same gain (no intra-frame stereo imbalance).
        if self.channel_pos == 0 {
            for i in 0..NUM_STEMS {
                let target = f32::from_bits(self.gains[i].load(Ordering::Relaxed));
                let delta = target - self.ramp[i];
                self.ramp[i] = if delta.abs() <= self.ramp_step {
                    target
                } else {
                    self.ramp[i] + self.ramp_step.copysign(delta)
                };
            }
        }
        self.channel_pos = (self.channel_pos + 1) % self.channels.get();

        // Exhausted stems contribute silence; the mix ends only when every
        // stem has ended so a short stem never truncates the track.
        let mut acc = 0.0;
        let mut any_live = false;
        for i in 0..NUM_STEMS {
            if self.done[i] {
                continue;
            }
            match self.sources[i].next() {
                Some(s) => {
                    any_live = true;
                    acc += s * self.ramp[i];
                }
                None => self.done[i] = true,
            }
        }
        if any_live {
            Some(acc)
        } else {
            None
        }
    }
}

impl<S: Source> Source for StemMixerSource<S> {
    fn current_span_len(&self) -> Option<usize> {
        // Uniform parameters for the whole stream; length is decoder-
        // dependent and unknown upfront.
        None
    }

    fn channels(&self) -> ChannelCount {
        self.channels
    }

    fn sample_rate(&self) -> SampleRate {
        self.sample_rate
    }

    fn total_duration(&self) -> Option<Duration> {
        self.total
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rodio::buffer::SamplesBuffer;
    use std::num::NonZero;
    use zytunes::stems::{new_stem_gains, set_stem_gain};

    fn mono(data: Vec<f32>) -> SamplesBuffer {
        SamplesBuffer::new(
            NonZero::new(1).unwrap(),
            NonZero::new(44_100).unwrap(),
            data,
        )
    }

    /// Six mono stems where stem `i` is a constant `(i + 1) / 100` signal
    /// of `len` samples.
    fn six_constant_stems(len: usize) -> [SamplesBuffer; NUM_STEMS] {
        std::array::from_fn(|i| mono(vec![(i as f32 + 1.0) / 100.0; len]))
    }

    const ALL_ON: [bool; NUM_STEMS] = [true; NUM_STEMS];

    #[test]
    fn mix_sums_all_enabled_stems() {
        let mixer = StemMixerSource::new(six_constant_stems(8), new_stem_gains(&ALL_ON)).unwrap();
        let out: Vec<f32> = mixer.collect();
        assert_eq!(out.len(), 8);
        let expected = (1..=NUM_STEMS).map(|i| i as f32 / 100.0).sum::<f32>();
        for s in out {
            assert!((s - expected).abs() < 1e-6, "expected {expected}, got {s}");
        }
    }

    #[test]
    fn stem_muted_at_construction_contributes_nothing() {
        let mut enabled = ALL_ON;
        enabled[0] = false; // drop the 0.01 stem
        let mixer = StemMixerSource::new(six_constant_stems(4), new_stem_gains(&enabled)).unwrap();
        let out: Vec<f32> = mixer.collect();
        let expected = (2..=NUM_STEMS).map(|i| i as f32 / 100.0).sum::<f32>();
        for s in out {
            assert!((s - expected).abs() < 1e-6, "expected {expected}, got {s}");
        }
    }

    #[test]
    fn live_toggle_ramps_without_step_discontinuity() {
        // Stem 0 carries a constant 1.0; the rest are silent but present.
        let mut stems = six_constant_stems(44_100);
        stems[0] = mono(vec![1.0; 44_100]);
        for s in stems.iter_mut().skip(1) {
            *s = mono(vec![0.0; 44_100]);
        }
        let gains = new_stem_gains(&ALL_ON);
        let mut mixer = StemMixerSource::new(stems, gains.clone()).unwrap();

        // Steady state before the toggle.
        for _ in 0..100 {
            assert_eq!(mixer.next(), Some(1.0));
        }

        set_stem_gain(&gains, 0, false);

        // The output must descend monotonically to 0 with bounded steps —
        // a >step jump is an audible click. Full transition within
        // RAMP_SECONDS worth of frames (mono: 441) plus slack.
        let max_step = 1.0 / (RAMP_SECONDS * 44_100.0) + 1e-6;
        let mut prev = 1.0f32;
        let mut reached_zero_at = None;
        for n in 0..600 {
            let s = mixer.next().expect("mid-track");
            assert!(s <= prev + 1e-6, "gain must not bounce back up");
            assert!(
                prev - s <= max_step,
                "step {} exceeds ramp increment {max_step} at sample {n}",
                prev - s
            );
            prev = s;
            if s == 0.0 {
                reached_zero_at = Some(n);
                break;
            }
        }
        let n = reached_zero_at.expect("must reach silence");
        assert!(n <= 442, "ramp took {n} samples; expected ~441");

        // And back up again — same contract, rising.
        set_stem_gain(&gains, 0, true);
        let mut prev = 0.0f32;
        for _ in 0..600 {
            let s = mixer.next().expect("mid-track");
            assert!(s + 1e-6 >= prev, "gain must not dip while ramping up");
            assert!(s - prev <= max_step, "up-ramp step too large");
            prev = s;
            if s == 1.0 {
                return;
            }
        }
        panic!("never returned to full gain");
    }

    #[test]
    fn short_stems_pad_with_silence_until_longest_ends() {
        let mut stems = six_constant_stems(4);
        stems[0] = mono(vec![0.5; 8]); // twice as long as the rest
        let mixer = StemMixerSource::new(stems, new_stem_gains(&ALL_ON)).unwrap();
        let out: Vec<f32> = mixer.collect();
        assert_eq!(out.len(), 8, "mix ends with the longest stem");
        let full: f32 = 0.5 + (2..=NUM_STEMS).map(|i| i as f32 / 100.0).sum::<f32>();
        for s in &out[..4] {
            assert!((s - full).abs() < 1e-6);
        }
        for s in &out[4..] {
            assert!((s - 0.5).abs() < 1e-6, "tail is stem 0 alone, got {s}");
        }
    }

    #[test]
    fn all_muted_still_advances_and_ends() {
        // TrackEnded depends on the source ending even at full silence.
        let mixer =
            StemMixerSource::new(six_constant_stems(4), new_stem_gains(&[false; NUM_STEMS]))
                .unwrap();
        let out: Vec<f32> = mixer.collect();
        assert_eq!(out, vec![0.0; 4]);
    }

    #[test]
    fn mismatched_sample_rate_is_rejected() {
        let mut stems = six_constant_stems(4);
        stems[3] = SamplesBuffer::new(
            NonZero::new(1).unwrap(),
            NonZero::new(48_000).unwrap(),
            vec![0.0; 4],
        );
        let err = StemMixerSource::new(stems, new_stem_gains(&ALL_ON))
            .err()
            .expect("mismatch must be rejected");
        assert!(err.contains("stem 3"), "{err}");
    }

    #[test]
    fn mismatched_channel_count_is_rejected() {
        let mut stems = six_constant_stems(4);
        stems[5] = SamplesBuffer::new(
            NonZero::new(2).unwrap(),
            NonZero::new(44_100).unwrap(),
            vec![0.0; 4],
        );
        let err = StemMixerSource::new(stems, new_stem_gains(&ALL_ON))
            .err()
            .expect("mismatch must be rejected");
        assert!(err.contains("stem 5"), "{err}");
    }

    #[test]
    fn reports_uniform_source_parameters() {
        let stems: [SamplesBuffer; NUM_STEMS] = std::array::from_fn(|_| {
            SamplesBuffer::new(
                NonZero::new(2).unwrap(),
                NonZero::new(44_100).unwrap(),
                vec![0.0; 8],
            )
        });
        let reference = SamplesBuffer::new(
            NonZero::new(2).unwrap(),
            NonZero::new(44_100).unwrap(),
            vec![0.0; 8],
        );
        let mixer = StemMixerSource::new(stems, new_stem_gains(&ALL_ON)).unwrap();
        assert_eq!(mixer.channels().get(), 2);
        assert_eq!(mixer.sample_rate().get(), 44_100);
        // Longest (here: identical) stem's duration wins.
        assert_eq!(mixer.total_duration(), reference.total_duration());
    }
}
