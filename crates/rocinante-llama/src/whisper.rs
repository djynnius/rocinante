//! whisper.cpp speech recognition and the Silero voice-activity model, for
//! `/talk`. Both run in-process on the ggml llama.cpp already links (see
//! `shim/rcn_whisper.cpp` and build.rs).
//!
//! Neither context is thread-safe: each is `Send` (moved to the thread that
//! uses it) but not `Sync`, and every call takes `&mut self`.

use std::ffi::{CString, c_char};
use std::path::Path;
use std::ptr::NonNull;
use std::sync::OnceLock;

use crate::ffi::{LogFn, take_string};

#[repr(C)]
struct WhisperCtx {
    _private: [u8; 0],
}

#[repr(C)]
struct VadCtx {
    _private: [u8; 0],
}

unsafe extern "C" {
    fn rcn_whisper_init(log: Option<LogFn>);
    fn rcn_whisper_load(path: *const c_char, use_gpu: bool) -> *mut WhisperCtx;
    fn rcn_whisper_free(ctx: *mut WhisperCtx);
    fn rcn_whisper_transcribe(
        ctx: *mut WhisperCtx,
        samples: *const f32,
        n_samples: i32,
        n_threads: i32,
        prompt: *const c_char,
        no_speech: *mut f32,
    ) -> *mut c_char;
    fn rcn_vad_load(path: *const c_char) -> *mut VadCtx;
    fn rcn_vad_free(ctx: *mut VadCtx);
    fn rcn_vad_reset(ctx: *mut VadCtx);
    fn rcn_vad_probs(
        ctx: *mut VadCtx,
        samples: *const f32,
        n_samples: i32,
        out: *mut f32,
        max_out: i32,
    ) -> i32;
}

/// Sample rate both models expect: 16 kHz mono f32 in -1.0..=1.0.
pub const SAMPLE_RATE: u32 = 16_000;

fn ensure_init() {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {
        // SAFETY: stores a function pointer; called once before any load.
        unsafe { rcn_whisper_init(Some(crate::log_cb)) };
    });
}

fn c_path(path: &Path) -> Result<CString, String> {
    CString::new(path.to_string_lossy().as_bytes())
        .map_err(|_| format!("bad path: {}", path.display()))
}

fn cpu_check() -> Result<(), String> {
    match crate::missing_cpu_features() {
        Some(missing) => Err(format!(
            "this CPU lacks {missing}, which Rocinante's speech recognition needs \
             (x86-64 from 2013 on)"
        )),
        None => Ok(()),
    }
}

/// One transcribed utterance.
#[derive(Debug, Clone, PartialEq)]
pub struct Transcript {
    pub text: String,
    /// whisper's highest no-speech probability across segments: close to
    /// 1.0 when it heard no words (noise, a cough, a door).
    pub no_speech: f32,
}

/// A loaded whisper.cpp model (a `ggml-*.bin` file).
pub struct Whisper {
    ctx: NonNull<WhisperCtx>,
}

// SAFETY: the context is only touched through `&mut self`, so at most one
// thread uses it at a time; whisper.cpp keeps no thread-local state for it.
unsafe impl Send for Whisper {}

impl Whisper {
    /// Load a model. `use_gpu` lets ggml use the GPU backend when the build
    /// has one (Metal on Macs); elsewhere it falls back to the CPU.
    pub fn load(path: &Path, use_gpu: bool) -> Result<Self, String> {
        cpu_check()?;
        ensure_init();
        let c = c_path(path)?;
        // SAFETY: valid NUL-terminated path; null result means failure.
        let ptr = unsafe { rcn_whisper_load(c.as_ptr(), use_gpu) };
        NonNull::new(ptr)
            .map(|ctx| Self { ctx })
            .ok_or_else(|| format!("whisper.cpp could not load {}", path.display()))
    }

    /// Transcribe 16 kHz mono samples as English. `threads` 0 = whisper.cpp's
    /// default; `prompt` biases spelling (names, project words).
    pub fn transcribe(
        &mut self,
        samples: &[f32],
        threads: u32,
        prompt: Option<&str>,
    ) -> Result<Transcript, String> {
        if samples.is_empty() {
            return Ok(Transcript {
                text: String::new(),
                no_speech: 1.0,
            });
        }
        let n = i32::try_from(samples.len()).map_err(|_| "audio too long".to_string())?;
        let prompt = prompt
            .map(|p| CString::new(p.replace('\0', " ")).expect("NULs removed"))
            .unwrap_or_default();
        let mut no_speech = 0.0f32;
        // SAFETY: ctx is live and exclusively borrowed; samples/prompt
        // outlive the call; the result is a shim-allocated string or null.
        let text = unsafe {
            take_string(rcn_whisper_transcribe(
                self.ctx.as_ptr(),
                samples.as_ptr(),
                n,
                threads as i32,
                prompt.as_ptr(),
                &mut no_speech,
            ))
        }
        .ok_or_else(|| "whisper.cpp failed to transcribe the audio".to_string())?;
        Ok(Transcript {
            text: text.trim().to_string(),
            no_speech,
        })
    }
}

impl Drop for Whisper {
    fn drop(&mut self) {
        // SAFETY: freed exactly once.
        unsafe { rcn_whisper_free(self.ctx.as_ptr()) };
    }
}

/// The Silero voice-activity model (`ggml-silero-*.bin`), run on a live
/// stream: it keeps its state between calls until [`Vad::reset`].
pub struct Vad {
    ctx: NonNull<VadCtx>,
}

// SAFETY: as for `Whisper` — exclusive `&mut self` access only.
unsafe impl Send for Vad {}

impl Vad {
    /// Samples per probability: feed multiples of this.
    pub const WINDOW: usize = 512;

    pub fn load(path: &Path) -> Result<Self, String> {
        cpu_check()?;
        ensure_init();
        let c = c_path(path)?;
        // SAFETY: valid path; null result means failure.
        let ptr = unsafe { rcn_vad_load(c.as_ptr()) };
        NonNull::new(ptr).map(|ctx| Self { ctx }).ok_or_else(|| {
            format!(
                "whisper.cpp could not load the VAD model {}",
                path.display()
            )
        })
    }

    /// Speech probability (0..=1) of each [`Vad::WINDOW`]-sample window.
    /// A trailing partial window is zero-padded, so callers should keep
    /// leftovers for the next call instead.
    pub fn probs(&mut self, samples: &[f32]) -> Result<Vec<f32>, String> {
        if samples.is_empty() {
            return Ok(Vec::new());
        }
        let n = i32::try_from(samples.len()).map_err(|_| "audio too long".to_string())?;
        let max = samples.len().div_ceil(Self::WINDOW);
        let mut out = vec![0.0f32; max];
        // SAFETY: ctx is live and exclusively borrowed; `out` holds `max`
        // floats and the shim writes at most that many.
        let got = unsafe {
            rcn_vad_probs(
                self.ctx.as_ptr(),
                samples.as_ptr(),
                n,
                out.as_mut_ptr(),
                max as i32,
            )
        };
        if got < 0 {
            return Err("the voice-activity model failed".into());
        }
        out.truncate(got as usize);
        Ok(out)
    }

    /// Forget the stream so far (between utterances).
    pub fn reset(&mut self) {
        // SAFETY: ctx is live.
        unsafe { rcn_vad_reset(self.ctx.as_ptr()) };
    }
}

impl Drop for Vad {
    fn drop(&mut self) {
        // SAFETY: freed exactly once.
        unsafe { rcn_vad_free(self.ctx.as_ptr()) };
    }
}
