//! Record from the microphone as 16 kHz mono f32 chunks over a channel.
//! Dropping the [`Capture`] stops recording.
//!
//! macOS and Windows use the OS audio API (cpal) and resample here. Linux
//! asks the sound server for 16 kHz mono directly through `parecord` or
//! `arecord`: the release binary is static and cannot load the desktop's
//! audio libraries itself.

use std::sync::mpsc::Sender;
use std::thread::JoinHandle;

use crate::detect::{Backend, Mic};

pub struct Capture {
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    stop: Option<Sender<()>>,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    child: Option<std::process::Child>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Capture {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Start recording from `mic`; chunks arrive on `tx` until the Capture is
/// dropped or the device goes away (then `tx` is dropped).
pub fn start(mic: &Mic, tx: Sender<Vec<f32>>) -> Result<Capture, String> {
    match mic.backend {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        Backend::Native => native::start(tx),
        #[cfg(target_os = "linux")]
        Backend::Pulse => process::start(
            "parecord",
            &[
                "--raw",
                "--format=s16le",
                "--rate=16000",
                "--channels=1",
                "--latency-msec=60",
            ],
            tx,
        ),
        #[cfg(target_os = "linux")]
        Backend::Alsa => process::start(
            "arecord",
            &["-q", "-t", "raw", "-f", "S16_LE", "-r", "16000", "-c", "1"],
            tx,
        ),
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod native {
    use std::sync::mpsc::{self, Sender};
    use std::time::Duration;

    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use cpal::{FromSample, Sample, SizedSample};

    use super::Capture;
    use crate::resample::{Resampler, downmix};

    pub fn start(tx: Sender<Vec<f32>>) -> Result<Capture, String> {
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        // cpal streams are not Send: one thread builds, runs and drops it.
        let thread = std::thread::Builder::new()
            .name("rocinante-mic".into())
            .spawn(move || {
                let stream = match build(tx).and_then(|s| {
                    s.play()
                        .map_err(|e| format!("cannot start the microphone: {e}"))?;
                    Ok(s)
                }) {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                let _ = ready_tx.send(Ok(()));
                // Until Capture is dropped (stop sent, or the sender gone).
                let _ = stop_rx.recv();
                drop(stream);
            })
            .map_err(|e| format!("cannot start the microphone thread: {e}"))?;
        match ready_rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(())) => Ok(Capture {
                stop: Some(stop_tx),
                child: None,
                thread: Some(thread),
            }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => Err("the microphone did not start within 10 s".into()),
        }
    }

    fn build(tx: Sender<Vec<f32>>) -> Result<cpal::Stream, String> {
        let host = cpal::default_host();
        let device = host.default_input_device().ok_or("no microphone found")?;
        let supported = device
            .default_input_config()
            .map_err(|e| format!("cannot read the microphone's format: {e}"))?;
        let channels = supported.channels() as usize;
        let rate = supported.sample_rate();
        let config = supported.config();
        let err = |e: cpal::Error| tracing::warn!(target: "voice", "microphone: {e}");
        let stream = match supported.sample_format() {
            cpal::SampleFormat::F32 => {
                device.build_input_stream(config, callback::<f32>(channels, rate, tx), err, None)
            }
            cpal::SampleFormat::I16 => {
                device.build_input_stream(config, callback::<i16>(channels, rate, tx), err, None)
            }
            cpal::SampleFormat::U16 => {
                device.build_input_stream(config, callback::<u16>(channels, rate, tx), err, None)
            }
            cpal::SampleFormat::I32 => {
                device.build_input_stream(config, callback::<i32>(channels, rate, tx), err, None)
            }
            other => {
                return Err(format!(
                    "the microphone's sample format {other} isn't supported"
                ));
            }
        };
        stream.map_err(|e| format!("cannot open the microphone: {e}"))
    }

    fn callback<T>(
        channels: usize,
        rate: u32,
        tx: Sender<Vec<f32>>,
    ) -> impl FnMut(&[T], &cpal::InputCallbackInfo) + Send + 'static
    where
        T: SizedSample,
        f32: FromSample<T>,
    {
        let mut resampler = Resampler::new(rate, rocinante_llama::whisper::SAMPLE_RATE);
        let mut floats = Vec::new();
        let mut mono = Vec::new();
        move |data: &[T], _| {
            floats.clear();
            floats.extend(data.iter().map(|&s| f32::from_sample(s)));
            mono.clear();
            downmix(&floats, channels, &mut mono);
            let mut out = Vec::with_capacity(mono.len());
            resampler.process(&mono, &mut out);
            if !out.is_empty() {
                let _ = tx.send(out);
            }
        }
    }
}

#[cfg(target_os = "linux")]
mod process {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::sync::mpsc::Sender;

    use super::Capture;

    /// Run a recorder writing 16 kHz mono s16le to stdout.
    pub fn start(program: &str, args: &[&str], tx: Sender<Vec<f32>>) -> Result<Capture, String> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("cannot run {program}: {e}"))?;
        let mut stdout = child.stdout.take().expect("piped stdout");
        let thread = std::thread::Builder::new()
            .name("rocinante-mic".into())
            .spawn(move || {
                // 100 ms per read at 16 kHz × 2 bytes.
                let mut buf = vec![0u8; 3200];
                let mut carry: Option<u8> = None;
                loop {
                    let n = match stdout.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    let samples = super::s16le_to_f32(&buf[..n], &mut carry);
                    if !samples.is_empty() && tx.send(samples).is_err() {
                        break;
                    }
                }
            })
            .map_err(|e| format!("cannot start the microphone thread: {e}"))?;
        Ok(Capture {
            stop: None,
            child: Some(child),
            thread: Some(thread),
        })
    }
}

/// Little-endian 16-bit PCM to f32, carrying an odd trailing byte over to
/// the next read.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn s16le_to_f32(bytes: &[u8], carry: &mut Option<u8>) -> Vec<f32> {
    let mut joined;
    let data: &[u8] = match carry.take() {
        Some(b) => {
            joined = Vec::with_capacity(bytes.len() + 1);
            joined.push(b);
            joined.extend_from_slice(bytes);
            &joined
        }
        None => bytes,
    };
    let pairs = data.chunks_exact(2);
    if let [b] = pairs.remainder() {
        *carry = Some(*b);
    }
    pairs
        .map(|p| i16::from_le_bytes([p[0], p[1]]) as f32 / 32768.0)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_pcm_across_odd_reads() {
        let mut carry = None;
        // 0x4000 = 0.5, split across two reads.
        assert_eq!(s16le_to_f32(&[0x00, 0x40, 0x00], &mut carry), vec![0.5]);
        assert_eq!(carry, Some(0x00));
        assert_eq!(s16le_to_f32(&[0xC0], &mut carry), vec![-0.5]);
        assert_eq!(carry, None);
    }
}
