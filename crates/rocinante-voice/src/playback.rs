//! Play mono audio through the speakers, and stop it instantly.
//!
//! macOS and Windows use the OS audio API (cpal). Linux pipes 16-bit PCM
//! into `pacat` or `aplay` (the release binary is static and cannot load
//! the desktop's audio libraries); stopping kills the player, which drops
//! whatever the sound server still had buffered.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use crate::detect::{Backend, Speakers};

/// Audio waiting to be played, at the output's rate.
#[derive(Default)]
struct Queue {
    samples: VecDeque<f32>,
}

pub struct Player {
    queue: Arc<Mutex<Queue>>,
    inner: Inner,
}

enum Inner {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    Native(native::Output),
    #[cfg(target_os = "linux")]
    Process(process::Output),
}

impl Player {
    /// Open the speakers for mono audio at `rate` Hz.
    pub fn start(speakers: &Speakers, rate: u32) -> Result<Self, String> {
        let queue = Arc::new(Mutex::new(Queue::default()));
        let inner = match speakers.backend {
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            Backend::Native => Inner::Native(native::Output::start(rate, queue.clone())?),
            #[cfg(target_os = "linux")]
            Backend::Pulse => Inner::Process(process::Output::new(
                "pacat",
                &[
                    "--playback",
                    "--raw",
                    "--format=s16le",
                    "--channels=1",
                    "--latency-msec=80",
                    "--client-name=rocinante",
                ],
                "--rate",
                rate,
                queue.clone(),
            )),
            #[cfg(target_os = "linux")]
            Backend::Alsa => Inner::Process(process::Output::new(
                "aplay",
                &["-q", "-t", "raw", "-f", "S16_LE", "-c", "1"],
                "-r",
                rate,
                queue.clone(),
            )),
        };
        Ok(Self { queue, inner })
    }

    /// Queue `pcm` (mono, at the rate given to `start`).
    pub fn play(&self, pcm: &[f32]) {
        match &self.inner {
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            Inner::Native(out) => out.play(pcm, &self.queue),
            #[cfg(target_os = "linux")]
            Inner::Process(out) => out.play(pcm, &self.queue),
        }
    }

    /// Drop everything queued and silence the output now.
    pub fn stop(&self) {
        self.queue.lock().unwrap().samples.clear();
        match &self.inner {
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            Inner::Native(out) => out.stop(),
            #[cfg(target_os = "linux")]
            Inner::Process(out) => out.stop(),
        }
    }

    /// Nothing queued and nothing still sounding.
    pub fn is_idle(&self) -> bool {
        if !self.queue.lock().unwrap().samples.is_empty() {
            return false;
        }
        match &self.inner {
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            Inner::Native(_) => true,
            #[cfg(target_os = "linux")]
            Inner::Process(out) => out.is_idle(),
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod native {
    use std::sync::mpsc::{self, Sender};
    use std::sync::{Arc, Mutex};
    use std::thread::JoinHandle;
    use std::time::Duration;

    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use cpal::{FromSample, SizedSample};

    use super::Queue;
    use crate::resample::Resampler;

    pub struct Output {
        device_rate: u32,
        resampler: Mutex<Resampler>,
        source_rate: u32,
        stop: Option<Sender<()>>,
        thread: Option<JoinHandle<()>>,
    }

    impl Output {
        pub fn start(rate: u32, queue: Arc<Mutex<Queue>>) -> Result<Self, String> {
            let (ready_tx, ready_rx) = mpsc::channel::<Result<u32, String>>();
            let (stop_tx, stop_rx) = mpsc::channel::<()>();
            // cpal streams are not Send: one thread builds, runs and drops it.
            let thread = std::thread::Builder::new()
                .name("rocinante-speaker".into())
                .spawn(move || {
                    let built = build(queue).and_then(|(stream, device_rate)| {
                        stream
                            .play()
                            .map_err(|e| format!("cannot start the speakers: {e}"))?;
                        Ok((stream, device_rate))
                    });
                    let stream = match built {
                        Ok((stream, device_rate)) => {
                            let _ = ready_tx.send(Ok(device_rate));
                            stream
                        }
                        Err(e) => {
                            let _ = ready_tx.send(Err(e));
                            return;
                        }
                    };
                    let _ = stop_rx.recv();
                    drop(stream);
                })
                .map_err(|e| format!("cannot start the speaker thread: {e}"))?;
            match ready_rx.recv_timeout(Duration::from_secs(10)) {
                Ok(Ok(device_rate)) => Ok(Self {
                    device_rate,
                    resampler: Mutex::new(Resampler::new(rate, device_rate)),
                    source_rate: rate,
                    stop: Some(stop_tx),
                    thread: Some(thread),
                }),
                Ok(Err(e)) => {
                    let _ = thread.join();
                    Err(e)
                }
                Err(_) => Err("the speakers did not start within 10 s".into()),
            }
        }

        pub fn play(&self, pcm: &[f32], queue: &Mutex<Queue>) {
            let mut out = Vec::with_capacity(pcm.len() * 2);
            self.resampler.lock().unwrap().process(pcm, &mut out);
            queue.lock().unwrap().samples.extend(out);
        }

        pub fn stop(&self) {
            // Fresh interpolation history for the next reply.
            *self.resampler.lock().unwrap() = Resampler::new(self.source_rate, self.device_rate);
        }
    }

    impl Drop for Output {
        fn drop(&mut self) {
            if let Some(stop) = self.stop.take() {
                let _ = stop.send(());
            }
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    fn build(queue: Arc<Mutex<Queue>>) -> Result<(cpal::Stream, u32), String> {
        let device = cpal::default_host()
            .default_output_device()
            .ok_or("no speakers found")?;
        let supported = device
            .default_output_config()
            .map_err(|e| format!("cannot read the speakers' format: {e}"))?;
        let channels = supported.channels() as usize;
        let rate = supported.sample_rate();
        let config = supported.config();
        let err = |e: cpal::Error| tracing::warn!(target: "voice", "speakers: {e}");
        let stream = match supported.sample_format() {
            cpal::SampleFormat::F32 => {
                device.build_output_stream(config, fill::<f32>(channels, queue), err, None)
            }
            cpal::SampleFormat::I16 => {
                device.build_output_stream(config, fill::<i16>(channels, queue), err, None)
            }
            cpal::SampleFormat::U16 => {
                device.build_output_stream(config, fill::<u16>(channels, queue), err, None)
            }
            cpal::SampleFormat::I32 => {
                device.build_output_stream(config, fill::<i32>(channels, queue), err, None)
            }
            other => {
                return Err(format!(
                    "the speakers' sample format {other} isn't supported"
                ));
            }
        };
        stream
            .map(|s| (s, rate))
            .map_err(|e| format!("cannot open the speakers: {e}"))
    }

    /// Fill each frame from the queue (mono to every channel), silence when
    /// it runs dry.
    fn fill<T>(
        channels: usize,
        queue: Arc<Mutex<Queue>>,
    ) -> impl FnMut(&mut [T], &cpal::OutputCallbackInfo) + Send + 'static
    where
        T: SizedSample + FromSample<f32>,
    {
        move |data: &mut [T], _| {
            let mut q = queue.lock().unwrap();
            for frame in data.chunks_mut(channels.max(1)) {
                let s = q.samples.pop_front().unwrap_or(0.0).clamp(-1.0, 1.0);
                frame.fill(T::from_sample(s));
            }
        }
    }
}

#[cfg(target_os = "linux")]
mod process {
    use std::io::Write;
    use std::process::{Child, Command, Stdio};
    use std::sync::{Arc, Condvar, Mutex};
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    use super::Queue;

    /// What the writer thread shares with the player.
    struct State {
        child: Option<Child>,
        /// When the audio written so far finishes sounding (estimate).
        sounding_until: Instant,
        shutdown: bool,
    }

    pub struct Output {
        state: Arc<(Mutex<State>, Condvar)>,
        thread: Option<JoinHandle<()>>,
    }

    /// The player's own buffer: audio written may still sound this long.
    const LATENCY: Duration = Duration::from_millis(120);
    /// Samples written per write.
    const CHUNK: usize = 1_200;

    impl Output {
        pub fn new(
            program: &'static str,
            args: &'static [&'static str],
            rate_flag: &'static str,
            rate: u32,
            queue: Arc<Mutex<Queue>>,
        ) -> Self {
            let state = Arc::new((
                Mutex::new(State {
                    child: None,
                    sounding_until: Instant::now(),
                    shutdown: false,
                }),
                Condvar::new(),
            ));
            let thread = {
                let state = state.clone();
                std::thread::Builder::new()
                    .name("rocinante-speaker".into())
                    .spawn(move || writer(program, args, rate_flag, rate, queue, &state))
                    .ok()
            };
            Self { state, thread }
        }

        pub fn play(&self, pcm: &[f32], queue: &Mutex<Queue>) {
            queue.lock().unwrap().samples.extend(pcm);
            self.state.1.notify_all();
        }

        pub fn stop(&self) {
            let mut st = self.state.0.lock().unwrap();
            if let Some(mut child) = st.child.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
            st.sounding_until = Instant::now();
        }

        pub fn is_idle(&self) -> bool {
            Instant::now() >= self.state.0.lock().unwrap().sounding_until + LATENCY
        }
    }

    impl Drop for Output {
        fn drop(&mut self) {
            self.stop();
            self.state.0.lock().unwrap().shutdown = true;
            self.state.1.notify_all();
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    fn writer(
        program: &str,
        args: &[&str],
        rate_flag: &str,
        rate: u32,
        queue: Arc<Mutex<Queue>>,
        state: &(Mutex<State>, Condvar),
    ) {
        loop {
            let chunk: Vec<f32> = {
                let mut st = state.0.lock().unwrap();
                loop {
                    if st.shutdown {
                        return;
                    }
                    let mut q = queue.lock().unwrap();
                    if !q.samples.is_empty() {
                        let n = q.samples.len().min(CHUNK);
                        break q.samples.drain(..n).collect();
                    }
                    drop(q);
                    st = state
                        .1
                        .wait_timeout(st, Duration::from_millis(100))
                        .unwrap()
                        .0;
                }
            };
            let bytes: Vec<u8> = chunk
                .iter()
                .flat_map(|s| ((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes())
                .collect();
            // Take the player's stdin out of the lock while writing: a full
            // buffer blocks the write, and stop() must still get in.
            let stdin = {
                let mut st = state.0.lock().unwrap();
                if st.child.is_none() {
                    let rate_arg = rate.to_string();
                    st.child = Command::new(program)
                        .args(args)
                        .args([rate_flag, rate_arg.as_str()])
                        .stdin(Stdio::piped())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .spawn()
                        .map_err(|e| tracing::warn!(target: "voice", "cannot run {program}: {e}"))
                        .ok();
                }
                st.child.as_mut().and_then(|c| c.stdin.take())
            };
            let Some(mut stdin) = stdin else {
                continue;
            };
            let ok = stdin.write_all(&bytes).is_ok();
            let mut st = state.0.lock().unwrap();
            let secs = chunk.len() as f64 / rate as f64;
            let now = Instant::now();
            st.sounding_until = st.sounding_until.max(now) + Duration::from_secs_f64(secs);
            // Hand stdin back unless stop() killed the child meanwhile.
            if ok
                && let Some(child) = st.child.as_mut()
                && child.stdin.is_none()
            {
                child.stdin = Some(stdin);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    /// Needs real speakers, and plays only silence:
    /// `cargo test -p rocinante-voice -- --ignored speakers`.
    #[test]
    #[ignore]
    fn speakers_drain_queued_audio_and_stop_clears_it() {
        let speakers = crate::detect::find_speakers().expect("speakers");
        let player = Player::start(&speakers, 24_000).expect("open speakers");
        assert!(player.is_idle());
        player.play(&vec![0.0; 12_000]); // 0.5 s
        assert!(!player.is_idle(), "queued");
        let t = Instant::now();
        while !player.is_idle() {
            assert!(
                t.elapsed() < Duration::from_secs(3),
                "the device never drained"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let took = t.elapsed();
        assert!(
            took >= Duration::from_millis(300),
            "drained too fast: {took:?}"
        );
        player.play(&vec![0.0; 48_000]); // 2 s
        player.stop();
        assert!(player.is_idle(), "stop drops the queue at once");
        println!("{}: 0.5 s drained in {took:?}", speakers.name);
    }
}
