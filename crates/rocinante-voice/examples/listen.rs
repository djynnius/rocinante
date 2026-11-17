//! Drive the `/talk` listener by hand.
//!
//! From the microphone (speak; Ctrl+C to stop):
//!   cargo run --release -p rocinante-voice --example listen
//!
//! From a 16 kHz mono 16-bit WAV, streamed in real-time-sized chunks with
//! silence and noise around it (no microphone needed):
//!   cargo run --release -p rocinante-voice --example listen -- speech.wav
//!
//! Models come from ~/.rocinante/voice (downloaded on first run).

use std::sync::mpsc;
use std::time::{Duration, Instant};

use rocinante_voice::models::{self, SttModel};
use rocinante_voice::{Listener, ListenerEvent, ListenerOptions, prompt_for};

#[tokio::main]
async fn main() {
    let wav = std::env::args().nth(1);
    let dir = models::dir().expect("home directory");
    let model = SttModel::parse(&std::env::var("STT").unwrap_or_default()).unwrap_or_default();
    let mut paths = Vec::new();
    for file in [models::VAD, model.file()] {
        let path = models::fetch(&dir, &file, |done, total| {
            eprint!(
                "\r{} {}/{}",
                file.file,
                models::megabytes(done),
                models::megabytes(total)
            );
        })
        .await
        .expect("model download");
        eprintln!("\r{} ready{:30}", file.file, "");
        paths.push(path);
    }
    let opts = ListenerOptions {
        vad: paths[0].clone(),
        model: paths[1].clone(),
        prompt: prompt_for(Some("Donald")),
        pause_ms: rocinante_voice::DEFAULT_PAUSE_MS,
    };
    let t0 = Instant::now();
    let events = move |e: ListenerEvent| println!("{:6.2}s  {e:?}", t0.elapsed().as_secs_f32());

    match wav {
        None => {
            let mic = rocinante_voice::check().unwrap_or_else(|e| panic!("{e}"));
            println!("listening on {} — speak; Ctrl+C stops", mic.name);
            let _listener = Listener::start(&mic, opts, events).expect("start");
            tokio::signal::ctrl_c().await.ok();
        }
        Some(path) => {
            let speech = read_wav_16k(&path);
            let (tx, rx) = mpsc::channel();
            let listener = Listener::start_with_audio(opts, rx, events).expect("start");
            // 1 s silence, the speech, 1.5 s faint noise, the speech again,
            // 2 s silence — in 100 ms chunks at real-time pace.
            let mut stream = vec![0.0f32; 16_000];
            stream.extend(&speech);
            stream.extend(noise(24_000, 0.003));
            stream.extend(&speech);
            stream.extend(vec![0.0f32; 32_000]);
            for chunk in stream.chunks(1_600) {
                tx.send(chunk.to_vec()).unwrap();
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
            drop(listener);
        }
    }
}

fn noise(n: usize, level: f32) -> Vec<f32> {
    let mut x: u32 = 0x1234_5678;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x as f32 / u32::MAX as f32 - 0.5) * 2.0 * level
        })
        .collect()
}

fn read_wav_16k(path: &str) -> Vec<f32> {
    let bytes = std::fs::read(path).expect("read wav");
    let mut pos = 12;
    while pos + 8 <= bytes.len() {
        let len = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
        if &bytes[pos..pos + 4] == b"fmt " {
            let rate = u32::from_le_bytes(bytes[pos + 12..pos + 16].try_into().unwrap());
            assert_eq!(rate, 16_000, "WAV must be 16 kHz");
        }
        if &bytes[pos..pos + 4] == b"data" {
            return bytes[pos + 8..pos + 8 + len]
                .chunks_exact(2)
                .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
                .collect();
        }
        pos += 8 + len + (len & 1);
    }
    panic!("no data chunk");
}
