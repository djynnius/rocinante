//! The listening loop: microphone → voice activity → utterances → whisper →
//! text. One worker thread runs both models; the microphone feeds it over a
//! channel, so audio keeps buffering while a sentence is transcribed.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use rocinante_llama::whisper::{SAMPLE_RATE, Vad, Whisper};

use crate::capture::{self, Capture};
use crate::detect::Mic;
use crate::endpoint::{EndpointConfig, Endpointer, Step, WINDOW};
use crate::filter;

pub struct ListenerOptions {
    /// The whisper.cpp model file.
    pub model: PathBuf,
    /// The Silero voice-activity model file.
    pub vad: PathBuf,
    /// Spelling hints for whisper (the speaker's name, "Rocinante").
    pub prompt: String,
    /// Quiet that ends an utterance, in milliseconds.
    pub pause_ms: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ListenerEvent {
    /// Speech started.
    Hearing,
    /// Speech ended; whisper is transcribing it.
    Transcribing,
    /// What was said.
    Heard(String),
    /// Something speech-like that held no words (a cough, a door).
    Ignored,
    /// Seconds of pure digital silence: the microphone is muted, or (macOS)
    /// the terminal has no microphone permission.
    Muted,
    /// Listening stopped for good.
    Failed(String),
}

/// A running listener. Dropping it stops the microphone and the worker.
pub struct Listener {
    stop: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    prompt: Arc<Mutex<String>>,
    capture: Option<Capture>,
    worker: Option<JoinHandle<()>>,
}

/// Exactly-zero audio for this long means the input is muted, not quiet:
/// a real microphone always has some noise.
const MUTED_AFTER: usize = SAMPLE_RATE as usize * 3;

impl Listener {
    /// Load both models and open the microphone. Blocking (the models take
    /// a fraction of a second; the very first run on a Mac also compiles
    /// GPU shaders) — call it from a blocking thread. `events` is called
    /// from the worker thread.
    pub fn start(
        mic: &Mic,
        opts: ListenerOptions,
        events: impl Fn(ListenerEvent) + Send + 'static,
    ) -> Result<Self, String> {
        let (tx, rx) = mpsc::channel::<Vec<f32>>();
        let models = load(&opts)?;
        let capture = capture::start(mic, tx)?;
        Self::run(models, opts, rx, Some(capture), events)
    }

    /// Listen to `audio` (16 kHz mono chunks) instead of a microphone: for
    /// tests and file input. Ends with `Failed` when the sender is dropped.
    pub fn start_with_audio(
        opts: ListenerOptions,
        audio: mpsc::Receiver<Vec<f32>>,
        events: impl Fn(ListenerEvent) + Send + 'static,
    ) -> Result<Self, String> {
        let models = load(&opts)?;
        Self::run(models, opts, audio, None, events)
    }

    fn run(
        (vad, whisper): (Vad, Whisper),
        opts: ListenerOptions,
        rx: mpsc::Receiver<Vec<f32>>,
        capture: Option<Capture>,
        events: impl Fn(ListenerEvent) + Send + 'static,
    ) -> Result<Self, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let paused = Arc::new(AtomicBool::new(false));
        let prompt = Arc::new(Mutex::new(opts.prompt));
        let worker = {
            let (stop, paused, prompt) = (stop.clone(), paused.clone(), prompt.clone());
            let cfg = EndpointConfig::default().with_pause_ms(opts.pause_ms);
            std::thread::Builder::new()
                .name("rocinante-listen".into())
                .spawn(move || {
                    Worker {
                        vad,
                        whisper,
                        endpointer: Endpointer::new(cfg),
                        prompt,
                        events: Box::new(events),
                    }
                    .run(rx, &stop, &paused)
                })
                .map_err(|e| format!("cannot start the listening thread: {e}"))?
        };
        Ok(Self {
            stop,
            paused,
            prompt,
            capture,
            worker: Some(worker),
        })
    }

    /// Ignore the microphone (true) or listen again (false).
    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::Relaxed);
    }

    /// Change whisper's spelling hints (e.g. a new speaker name).
    pub fn set_prompt(&self, prompt: String) {
        *self.prompt.lock().unwrap() = prompt;
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Stops the microphone, which also disconnects the worker's channel.
        drop(self.capture.take());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn load(opts: &ListenerOptions) -> Result<(Vad, Whisper), String> {
    Ok((Vad::load(&opts.vad)?, Whisper::load(&opts.model, true)?))
}

struct Worker {
    vad: Vad,
    whisper: Whisper,
    endpointer: Endpointer,
    prompt: Arc<Mutex<String>>,
    events: Box<dyn Fn(ListenerEvent) + Send>,
}

impl Worker {
    fn run(mut self, rx: mpsc::Receiver<Vec<f32>>, stop: &AtomicBool, paused: &AtomicBool) {
        let mut pending: Vec<f32> = Vec::new();
        let mut zeros = 0usize;
        let mut mute_check_done = false;
        loop {
            let chunk = match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(chunk) => chunk,
                Err(RecvTimeoutError::Timeout) if !stop.load(Ordering::Relaxed) => continue,
                Err(RecvTimeoutError::Timeout) => return,
                Err(RecvTimeoutError::Disconnected) => {
                    if !stop.load(Ordering::Relaxed) {
                        (self.events)(ListenerEvent::Failed(
                            "the microphone stopped sending audio (unplugged, or the recorder \
                             exited)"
                                .into(),
                        ));
                    }
                    return;
                }
            };
            if stop.load(Ordering::Relaxed) {
                return;
            }
            if paused.load(Ordering::Relaxed) {
                pending.clear();
                if self.endpointer.is_speaking() {
                    self.endpointer.reset();
                    self.vad.reset();
                }
                continue;
            }
            if !mute_check_done {
                if chunk.iter().all(|&s| s == 0.0) {
                    zeros += chunk.len();
                    if zeros >= MUTED_AFTER {
                        (self.events)(ListenerEvent::Muted);
                        mute_check_done = true;
                    }
                } else {
                    mute_check_done = true;
                }
            }
            pending.extend_from_slice(&chunk);
            let usable = pending.len() / WINDOW * WINDOW;
            if usable == 0 {
                continue;
            }
            let windows: Vec<f32> = pending.drain(..usable).collect();
            let probs = match self.vad.probs(&windows) {
                Ok(p) => p,
                Err(e) => {
                    (self.events)(ListenerEvent::Failed(e));
                    return;
                }
            };
            for (window, prob) in windows.chunks(WINDOW).zip(probs) {
                match self.endpointer.push(window, prob) {
                    Step::Nothing => {}
                    Step::Started => (self.events)(ListenerEvent::Hearing),
                    Step::Discarded => {
                        self.vad.reset();
                        (self.events)(ListenerEvent::Ignored);
                    }
                    Step::Utterance { samples, .. } => {
                        self.vad.reset();
                        (self.events)(ListenerEvent::Transcribing);
                        let heard = self.transcribe(&samples);
                        (self.events)(heard);
                    }
                }
            }
        }
    }

    fn transcribe(&mut self, samples: &[f32]) -> ListenerEvent {
        let prompt = self.prompt.lock().unwrap().clone();
        match self.whisper.transcribe(samples, 0, Some(&prompt)) {
            Ok(t) => match filter::clean(&t.text) {
                Some(text) => ListenerEvent::Heard(text),
                None => ListenerEvent::Ignored,
            },
            Err(e) => {
                tracing::warn!(target: "voice", "transcription failed: {e}");
                ListenerEvent::Ignored
            }
        }
    }
}

/// Whisper's spelling hints for a conversation with `name`.
pub fn prompt_for(name: Option<&str>) -> String {
    match name {
        Some(name) => format!("Hi Rocinante, it's {name}."),
        None => "Hi Rocinante.".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_names_the_speaker() {
        assert_eq!(prompt_for(Some("Donald")), "Hi Rocinante, it's Donald.");
        assert_eq!(prompt_for(None), "Hi Rocinante.");
    }
}
