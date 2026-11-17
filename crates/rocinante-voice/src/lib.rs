//! `/talk`: hands-free speech input for Rocinante, and replies read aloud.
//!
//! The microphone streams into the Silero voice-activity model, which cuts
//! speech into utterances at natural pauses; whisper.cpp turns each one into
//! text. Both models run in-process on the ggml that the embedded llama.cpp
//! already links (see rocinante-llama), and are downloaded on first use
//! into `~/.rocinante/voice/`, checked against pinned SHA-256 hashes.
//!
//! [`check`] says up front whether this machine can listen at all (no SSH
//! session, a supported CPU, a microphone, and on Linux a recording tool).
//!
//! Replies are spoken by Phonon ([`Speaker`]), Gradium's small on-device
//! TTS model, when [`find_speakers`] finds an output device.

mod capture;
mod detect;
mod endpoint;
mod filter;
mod listener;
pub mod models;
mod playback;
// Linux gets 16 kHz from the sound server and hands it 24 kHz directly.
#[cfg(any(target_os = "macos", target_os = "windows", test))]
mod resample;
mod speak;
pub mod speech_text;

pub use detect::{Mic, Speakers, Unavailable, check, find_speakers};
pub use listener::{Listener, ListenerEvent, ListenerOptions, prompt_for};
pub use models::SttModel;
pub use speak::{Speaker, SpeakerEvent};

/// Default quiet (ms) that ends an utterance.
pub const DEFAULT_PAUSE_MS: u32 = 800;
