//! Streaming sample-rate conversion to whisper's 16 kHz: a windowed-sinc
//! interpolator whose cutoff sits just under the lower rate's Nyquist
//! frequency, so a 48 kHz microphone does not alias hiss into the speech
//! band. 32 taps per output sample at 16 kHz is ~0.5 M multiply-adds per
//! second: nothing, even on old CPUs.

use std::f64::consts::PI;

/// Taps on each side of the interpolation point.
const HALF: usize = 16;

pub struct Resampler {
    /// Input samples per output sample.
    step: f64,
    /// Cutoff as a fraction of the input Nyquist (≤ 1).
    cutoff: f64,
    /// Input not yet fully consumed (includes `HALF` samples of history).
    buf: Vec<f32>,
    /// Position of the next output sample, in input samples from `buf[0]`.
    pos: f64,
}

impl Resampler {
    pub fn new(from_hz: u32, to_hz: u32) -> Self {
        let step = from_hz as f64 / to_hz as f64;
        Self {
            step,
            // 0.9 leaves room for the window's transition band.
            cutoff: (to_hz as f64 / from_hz as f64).min(1.0) * 0.9,
            buf: Vec::new(),
            pos: 0.0,
        }
    }

    pub fn is_passthrough(&self) -> bool {
        self.step == 1.0
    }

    /// Feed `input`, appending every output sample it completes to `out`.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if self.is_passthrough() {
            out.extend_from_slice(input);
            return;
        }
        self.buf.extend_from_slice(input);
        let len = self.buf.len();
        while self.pos + (HALF as f64) < len as f64 {
            let center = self.pos.floor() as isize;
            let frac = self.pos - center as f64;
            let (mut acc, mut norm) = (0.0f64, 0.0f64);
            for k in -(HALF as isize) + 1..=HALF as isize {
                let idx = center + k;
                if idx < 0 || idx as usize >= len {
                    continue;
                }
                let x = k as f64 - frac;
                let w = sinc(x * self.cutoff) * blackman(x / HALF as f64);
                acc += self.buf[idx as usize] as f64 * w;
                norm += w;
            }
            out.push(if norm.abs() > 1e-9 {
                (acc / norm) as f32
            } else {
                0.0
            });
            self.pos += self.step;
        }
        // Drop input no future output sample can reach.
        let keep_from = (self.pos.floor() as isize - HALF as isize).max(0) as usize;
        if keep_from > 0 {
            self.buf.drain(..keep_from.min(len));
            self.pos -= keep_from as f64;
        }
    }
}

fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-9 {
        1.0
    } else {
        (PI * x).sin() / (PI * x)
    }
}

/// Blackman window over -1..=1 (0 outside).
fn blackman(t: f64) -> f64 {
    if t.abs() >= 1.0 {
        return 0.0;
    }
    let u = (t + 1.0) / 2.0;
    0.42 - 0.5 * (2.0 * PI * u).cos() + 0.08 * (4.0 * PI * u).cos()
}

/// Average interleaved frames down to one channel.
pub fn downmix(interleaved: &[f32], channels: usize, out: &mut Vec<f32>) {
    if channels <= 1 {
        out.extend_from_slice(interleaved);
        return;
    }
    out.extend(
        interleaved
            .chunks_exact(channels)
            .map(|f| f.iter().sum::<f32>() / channels as f32),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(hz: f64, rate: u32, secs: f64) -> Vec<f32> {
        let n = (rate as f64 * secs) as usize;
        (0..n)
            .map(|i| (2.0 * PI * hz * i as f64 / rate as f64).sin() as f32)
            .collect()
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
    }

    /// Resample in uneven pieces, as a live stream arrives.
    fn run(input: &[f32], from: u32, to: u32) -> Vec<f32> {
        let mut r = Resampler::new(from, to);
        let mut out = Vec::new();
        for piece in input.chunks(997) {
            r.process(piece, &mut out);
        }
        out
    }

    fn zero_crossings(x: &[f32]) -> usize {
        x.windows(2)
            .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
            .count()
    }

    #[test]
    fn keeps_speech_band_tones_and_length() {
        for from in [48_000, 44_100, 22_050] {
            let input = sine(440.0, from, 1.0);
            let out = run(&input, from, 16_000);
            let expected = 16_000;
            assert!(
                (out.len() as i64 - expected).abs() <= 40,
                "{from}: {} samples",
                out.len()
            );
            // Skip the edges; a 440 Hz sine keeps its level and pitch.
            let mid = &out[500..out.len() - 500];
            assert!(
                (rms(mid) - std::f32::consts::FRAC_1_SQRT_2).abs() < 0.02,
                "{from}: rms {}",
                rms(mid)
            );
            let secs = mid.len() as f64 / 16_000.0;
            let hz = zero_crossings(mid) as f64 / 2.0 / secs;
            assert!((hz - 440.0).abs() < 5.0, "{from}: {hz} Hz");
        }
    }

    #[test]
    fn upsamples_speech_for_the_speakers() {
        // Phonon's 24 kHz to a 48 kHz (or 44.1 kHz) output.
        for to in [48_000, 44_100] {
            let out = run(&sine(440.0, 24_000, 1.0), 24_000, to);
            assert!(
                (out.len() as i64 - to as i64).abs() <= 40,
                "{to}: {}",
                out.len()
            );
            let mid = &out[500..out.len() - 500];
            assert!(
                (rms(mid) - std::f32::consts::FRAC_1_SQRT_2).abs() < 0.02,
                "{to}: rms {}",
                rms(mid)
            );
            let hz = zero_crossings(mid) as f64 / 2.0 / (mid.len() as f64 / to as f64);
            assert!((hz - 440.0).abs() < 5.0, "{to}: {hz} Hz");
        }
    }

    #[test]
    fn filters_tones_above_the_new_nyquist() {
        // 12 kHz cannot exist at 16 kHz; it must not fold down into speech.
        let out = run(&sine(12_000.0, 48_000, 1.0), 48_000, 16_000);
        assert!(rms(&out[500..out.len() - 500]) < 0.02);
    }

    #[test]
    fn passthrough_at_the_same_rate() {
        let input = sine(300.0, 16_000, 0.1);
        assert_eq!(run(&input, 16_000, 16_000), input);
    }

    #[test]
    fn downmixes_stereo() {
        let mut out = Vec::new();
        downmix(&[1.0, 0.0, 0.5, 0.5], 2, &mut out);
        assert_eq!(out, vec![0.5, 0.5]);
    }
}
