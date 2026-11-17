//! Cut a live 16 kHz stream into utterances from the voice-activity model's
//! per-window speech probabilities (one per 512 samples = 32 ms).
//!
//! Speech starts after a short run of confident windows; it ends after a
//! stretch of quiet. Hysteresis (a lower bar to stay in speech than to enter
//! it) keeps soft word endings and short pauses inside one utterance. A bit
//! of audio from just before the start is kept, so the first syllable is
//! not clipped.

use std::collections::VecDeque;

/// Samples per probability window (whisper.cpp's Silero model).
pub const WINDOW: usize = 512;
/// Milliseconds per window at 16 kHz.
pub const WINDOW_MS: u32 = 32;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EndpointConfig {
    /// Probability that counts as confident speech.
    pub start_prob: f32,
    /// Below this, a window counts as quiet once speech has started.
    pub end_prob: f32,
    /// Confident windows in a row that start an utterance.
    pub start_windows: u32,
    /// Quiet windows in a row that end it (25 × 32 ms = 0.8 s).
    pub end_windows: u32,
    /// Windows kept from before the start.
    pub preroll_windows: usize,
    /// Quiet windows kept after the last speech.
    pub tail_windows: usize,
    /// Utterances with fewer confident windows are noise (a click, a cough).
    pub min_speech_windows: u32,
    /// Hard cap: an utterance this long is cut and transcribed (whisper
    /// takes at most 30 s at once).
    pub max_windows: usize,
}

impl Default for EndpointConfig {
    fn default() -> Self {
        Self {
            start_prob: 0.5,
            end_prob: 0.35,
            start_windows: 3,
            end_windows: 25,
            preroll_windows: 10,
            tail_windows: 6,
            min_speech_windows: 8,
            max_windows: 28_000 / WINDOW_MS as usize,
        }
    }
}

impl EndpointConfig {
    /// End-of-speech pause in milliseconds (`[voice] pause_ms`), clamped to
    /// a usable range.
    pub fn with_pause_ms(mut self, ms: u32) -> Self {
        self.end_windows = (ms.clamp(300, 3000) / WINDOW_MS).max(1);
        self
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    Nothing,
    /// Speech just started (the UI shows "hearing you").
    Started,
    /// A finished utterance: 16 kHz samples, and seconds of confident speech
    /// in it.
    Utterance {
        samples: Vec<f32>,
        speech_secs: f32,
    },
    /// Something started but was too short to be words.
    Discarded,
}

pub struct Endpointer {
    cfg: EndpointConfig,
    speaking: bool,
    preroll: VecDeque<Vec<f32>>,
    utterance: Vec<f32>,
    /// Windows in the utterance, and where the last non-quiet one ended.
    windows: usize,
    last_voice_end: usize,
    confident_run: u32,
    quiet_run: u32,
    speech_windows: u32,
}

impl Endpointer {
    pub fn new(cfg: EndpointConfig) -> Self {
        Self {
            cfg,
            speaking: false,
            preroll: VecDeque::new(),
            utterance: Vec::new(),
            windows: 0,
            last_voice_end: 0,
            confident_run: 0,
            quiet_run: 0,
            speech_windows: 0,
        }
    }

    pub fn is_speaking(&self) -> bool {
        self.speaking
    }

    /// Forget everything (e.g. while listening is paused).
    pub fn reset(&mut self) {
        *self = Self::new(self.cfg);
    }

    /// Feed one [`WINDOW`]-sample window and its speech probability.
    pub fn push(&mut self, window: &[f32], prob: f32) -> Step {
        if !self.speaking {
            self.preroll.push_back(window.to_vec());
            while self.preroll.len() > self.cfg.preroll_windows.max(1) {
                self.preroll.pop_front();
            }
            if prob >= self.cfg.start_prob {
                self.confident_run += 1;
            } else {
                self.confident_run = 0;
            }
            if self.confident_run < self.cfg.start_windows {
                return Step::Nothing;
            }
            self.speaking = true;
            self.utterance = self.preroll.drain(..).flatten().collect();
            self.windows = self.utterance.len() / WINDOW;
            self.last_voice_end = self.windows;
            self.speech_windows = self.confident_run;
            self.quiet_run = 0;
            return Step::Started;
        }

        self.utterance.extend_from_slice(window);
        self.windows += 1;
        if prob >= self.cfg.start_prob {
            self.speech_windows += 1;
        }
        if prob >= self.cfg.end_prob {
            self.quiet_run = 0;
            self.last_voice_end = self.windows;
        } else {
            self.quiet_run += 1;
        }
        if self.quiet_run >= self.cfg.end_windows || self.windows >= self.cfg.max_windows {
            return self.finish();
        }
        Step::Nothing
    }

    fn finish(&mut self) -> Step {
        let keep = (self.last_voice_end + self.cfg.tail_windows).min(self.windows) * WINDOW;
        self.utterance.truncate(keep.min(self.utterance.len()));
        let samples = std::mem::take(&mut self.utterance);
        let speech_windows = self.speech_windows;
        self.reset();
        if speech_windows < self.cfg.min_speech_windows {
            return Step::Discarded;
        }
        Step::Utterance {
            samples,
            speech_secs: speech_windows as f32 * WINDOW_MS as f32 / 1000.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run a probability script through the endpointer; each window's
    /// samples carry its index so the cut points are checkable.
    fn script(probs: &[f32]) -> (Vec<Step>, Endpointer) {
        let mut ep = Endpointer::new(EndpointConfig::default());
        let steps = probs
            .iter()
            .enumerate()
            .map(|(i, &p)| ep.push(&vec![i as f32; WINDOW], p))
            .collect();
        (steps, ep)
    }

    fn window_ids(samples: &[f32]) -> Vec<usize> {
        samples.chunks(WINDOW).map(|w| w[0] as usize).collect()
    }

    #[test]
    fn silence_never_starts() {
        let (steps, ep) = script(&[0.1; 200]);
        assert!(steps.iter().all(|s| *s == Step::Nothing));
        assert!(!ep.is_speaking());
    }

    #[test]
    fn speech_then_pause_yields_one_utterance_with_preroll_and_tail() {
        // 20 quiet, 30 speech, 40 quiet.
        let mut probs = vec![0.05; 20];
        probs.extend([0.9; 30]);
        probs.extend([0.05; 40]);
        let (steps, _) = script(&probs);
        let started = steps.iter().position(|s| *s == Step::Started).unwrap();
        assert_eq!(started, 22, "third confident window");
        let utterances: Vec<_> = steps
            .iter()
            .filter_map(|s| match s {
                Step::Utterance {
                    samples,
                    speech_secs,
                } => Some((samples, *speech_secs)),
                _ => None,
            })
            .collect();
        assert_eq!(utterances.len(), 1);
        let (samples, secs) = utterances[0];
        let ids = window_ids(samples);
        // Preroll: the 10 windows up to and including the start window.
        assert_eq!(ids.first(), Some(&13));
        // Tail: 6 quiet windows after the last speech window (49).
        assert_eq!(ids.last(), Some(&55));
        assert!((secs - 30.0 * 0.032).abs() < 1e-3);
    }

    #[test]
    fn short_pauses_stay_inside_one_utterance() {
        let mut probs = vec![0.9; 20];
        probs.extend([0.1; 15]); // 0.48 s: a breath, not the end
        probs.extend([0.9; 20]);
        probs.extend([0.1; 30]);
        let (steps, _) = script(&probs);
        let n = steps
            .iter()
            .filter(|s| matches!(s, Step::Utterance { .. }))
            .count();
        assert_eq!(n, 1);
    }

    #[test]
    fn hysteresis_keeps_soft_word_endings() {
        // 0.4 is under the start bar but over the end bar: still speech.
        let mut probs = vec![0.9; 10];
        probs.extend([0.4; 40]);
        probs.extend([0.1; 30]);
        let (steps, _) = script(&probs);
        let Step::Utterance { samples, .. } = steps
            .iter()
            .find(|s| matches!(s, Step::Utterance { .. }))
            .unwrap()
        else {
            unreachable!()
        };
        assert!(window_ids(samples).contains(&49));
    }

    #[test]
    fn a_click_is_discarded() {
        let mut probs = vec![0.9; 4];
        probs.extend([0.05; 30]);
        let (steps, _) = script(&probs);
        assert!(steps.contains(&Step::Started));
        assert!(steps.contains(&Step::Discarded));
        assert!(!steps.iter().any(|s| matches!(s, Step::Utterance { .. })));
    }

    #[test]
    fn long_speech_is_cut_at_the_cap() {
        let cfg = EndpointConfig::default();
        let (steps, ep) = script(&vec![0.9; cfg.max_windows + 50]);
        let cut = steps
            .iter()
            .filter(|s| matches!(s, Step::Utterance { .. }))
            .count();
        assert_eq!(cut, 1);
        // The talker keeps going: a new utterance starts after the cut.
        assert!(ep.is_speaking());
    }

    #[test]
    fn pause_ms_sets_end_windows() {
        let cfg = EndpointConfig::default().with_pause_ms(1600);
        assert_eq!(cfg.end_windows, 50);
        assert_eq!(EndpointConfig::default().with_pause_ms(10).end_windows, 9);
    }
}
