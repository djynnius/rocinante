//! Read replies aloud with Phonon (Gradium's 31M-parameter on-device TTS,
//! pure Rust): ~10× faster than real time on one CPU core, so speech starts
//! within a few tens of milliseconds of a sentence being ready.
//!
//! One thread owns the model and the speakers. Sentences queue up and play
//! in order; [`Speaker::stop`] drops the queue and silences the output at
//! once (a generation counter abandons a sentence mid-synthesis).

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ptts::checkpoint::{Checkpoint, ResolveOptions};
use ptts::preprocess::Lang;
use ptts::synth::{DeviceKind, Quant, Synth};

use crate::detect::Speakers;
use crate::playback::Player;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpeakerEvent {
    /// Sound started (the microphone should stop listening).
    Speaking,
    /// All queued speech has played, plus a short tail for the room's echo.
    Done,
}

enum Cmd {
    Say { generation: u64, text: String },
}

pub struct Speaker {
    tx: Option<Sender<Cmd>>,
    generation: Arc<AtomicU64>,
    stop_now: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

/// Quiet kept after speech before the microphone listens again, so it does
/// not catch the reply's echo.
const ECHO_TAIL: Duration = Duration::from_millis(350);

impl Speaker {
    /// Load the Phonon checkpoint in `model_dir` and open the speakers.
    /// Blocking (the model loads in well under a second).
    pub fn start(
        model_dir: &Path,
        speakers: &Speakers,
        events: impl Fn(SpeakerEvent) + Send + 'static,
    ) -> Result<Self, String> {
        let synth = load(model_dir)?;
        let player = Player::start(speakers, synth.sample_rate())?;
        let (tx, rx) = mpsc::channel::<Cmd>();
        let generation = Arc::new(AtomicU64::new(0));
        let stop_now = Arc::new(AtomicBool::new(false));
        let thread = {
            let (generation, stop_now) = (generation.clone(), stop_now.clone());
            std::thread::Builder::new()
                .name("rocinante-speak".into())
                .spawn(move || run(synth, player, rx, &generation, &stop_now, events))
                .map_err(|e| format!("cannot start the speech thread: {e}"))?
        };
        Ok(Self {
            tx: Some(tx),
            generation,
            stop_now,
            thread: Some(thread),
        })
    }

    /// Queue a sentence.
    pub fn say(&self, text: String) {
        let generation = self.generation.load(Ordering::SeqCst);
        if let Some(tx) = &self.tx {
            let _ = tx.send(Cmd::Say { generation, text });
        }
    }

    /// Drop everything queued and stop talking now.
    pub fn stop(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.stop_now.store(true, Ordering::SeqCst);
    }
}

impl Drop for Speaker {
    fn drop(&mut self) {
        self.stop();
        self.tx.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn load(dir: &Path) -> Result<Synth, String> {
    let checkpoint = Checkpoint::resolve(
        dir,
        ResolveOptions {
            quant: Quant::Q4k,
            weights: None,
        },
    )
    .map_err(|e| format!("cannot load the Phonon voice in {}: {e}", dir.display()))?;
    let mut synth = checkpoint
        .builder(Lang::En)
        .device(DeviceKind::Cpu)
        .build()
        .map_err(|e| format!("cannot start the Phonon voice: {e}"))?;
    checkpoint.register_voices(&mut synth);
    Ok(synth)
}

fn run(
    synth: Synth,
    player: Player,
    rx: mpsc::Receiver<Cmd>,
    generation: &AtomicU64,
    stop_now: &AtomicBool,
    events: impl Fn(SpeakerEvent),
) {
    let mut speaking = false;
    let mut idle_since: Option<Instant> = None;
    loop {
        if stop_now.swap(false, Ordering::SeqCst) {
            player.stop();
        }
        match rx.recv_timeout(Duration::from_millis(40)) {
            Ok(Cmd::Say {
                generation: g,
                text,
            }) => {
                if g != generation.load(Ordering::SeqCst) {
                    continue;
                }
                let stream = match synth.stream(&text) {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!(target: "voice", "speech synthesis failed: {e}");
                        continue;
                    }
                };
                for chunk in stream {
                    if g != generation.load(Ordering::SeqCst) {
                        break;
                    }
                    match chunk {
                        Ok(pcm) if !pcm.is_empty() => {
                            if !speaking {
                                speaking = true;
                                events(SpeakerEvent::Speaking);
                            }
                            idle_since = None;
                            player.play(&pcm);
                        }
                        Ok(_) => {}
                        Err(e) => {
                            tracing::warn!(target: "voice", "speech synthesis failed: {e}");
                            break;
                        }
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                player.stop();
                return;
            }
        }
        if speaking && player.is_idle() {
            let since = *idle_since.get_or_insert_with(Instant::now);
            if since.elapsed() >= ECHO_TAIL {
                speaking = false;
                idle_since = None;
                events(SpeakerEvent::Done);
            }
        }
    }
}
