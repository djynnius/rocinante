//! Transcribe a WAV file with the embedded whisper.cpp, and show what the
//! voice-activity model hears in it:
//!
//!   cargo run --release -p rocinante-llama --example whisper_smoke -- \
//!       ggml-base.en-q5_1.bin ggml-silero-v6.2.0.bin speech.wav
//!
//! The WAV must be 16-bit PCM; any rate and channel count.

use std::path::Path;
use std::time::Instant;

use rocinante_llama::whisper::{SAMPLE_RATE, Vad, Whisper};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [model, vad, wav] = args.as_slice() else {
        eprintln!("usage: whisper_smoke <ggml model> <silero vad model> <file.wav>");
        std::process::exit(2);
    };
    let (rate, channels, pcm) = read_wav(Path::new(wav));
    let mono: Vec<f32> = pcm
        .chunks(channels)
        .map(|frame| frame.iter().map(|&s| s as f32 / 32768.0).sum::<f32>() / channels as f32)
        .collect();
    let samples = resample_linear(&mono, rate, SAMPLE_RATE);
    println!(
        "{wav}: {rate} Hz × {channels} → {} samples at 16 kHz ({:.2} s)",
        samples.len(),
        samples.len() as f32 / SAMPLE_RATE as f32
    );

    let t = Instant::now();
    let mut vad = Vad::load(Path::new(vad)).expect("load VAD");
    println!("vad load: {:.0} ms", t.elapsed().as_secs_f64() * 1000.0);
    let t = Instant::now();
    let probs = vad.probs(&samples[..Vad::WINDOW * 4]).expect("VAD");
    println!(
        "vad first 4 windows: {:.0} ms {probs:?}",
        t.elapsed().as_secs_f64() * 1000.0
    );
    vad.reset();
    let t = Instant::now();
    let probs = vad.probs(&samples).expect("VAD");
    let speech = probs.iter().filter(|&&p| p > 0.5).count();
    println!(
        "vad: {} windows, {speech} with speech (> 0.5), {:.0} ms",
        probs.len(),
        t.elapsed().as_secs_f64() * 1000.0
    );

    let t = Instant::now();
    let mut whisper = Whisper::load(Path::new(model), true).expect("load model");
    println!("load: {:.0} ms", t.elapsed().as_secs_f64() * 1000.0);
    for _ in 0..2 {
        let t = Instant::now();
        let out = whisper
            .transcribe(&samples, 0, std::env::var("PROMPT").ok().as_deref())
            .expect("transcribe");
        println!(
            "transcribe: {:.0} ms, no_speech {:.2}: {:?}",
            t.elapsed().as_secs_f64() * 1000.0,
            out.no_speech,
            out.text
        );
    }
    let silence = vec![0.0f32; SAMPLE_RATE as usize * 2];
    let out = whisper.transcribe(&silence, 0, None).expect("transcribe");
    println!(
        "2 s of silence: no_speech {:.2}: {:?}",
        out.no_speech, out.text
    );
}

fn read_wav(path: &Path) -> (u32, usize, Vec<i16>) {
    let bytes = std::fs::read(path).expect("read wav");
    assert_eq!(&bytes[0..4], b"RIFF", "not a WAV file");
    let (mut rate, mut channels, mut bits) = (0u32, 0usize, 0u16);
    let mut pos = 12;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let len = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let body = &bytes[pos + 8..(pos + 8 + len).min(bytes.len())];
        if id == b"fmt " {
            channels = u16::from_le_bytes([body[2], body[3]]) as usize;
            rate = u32::from_le_bytes(body[4..8].try_into().unwrap());
            bits = u16::from_le_bytes([body[14], body[15]]);
        } else if id == b"data" {
            assert_eq!(bits, 16, "only 16-bit PCM");
            let pcm = body
                .chunks_exact(2)
                .map(|b| i16::from_le_bytes([b[0], b[1]]))
                .collect();
            return (rate, channels, pcm);
        }
        pos += 8 + len + (len & 1);
    }
    panic!("no data chunk");
}

fn resample_linear(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() {
        return input.to_vec();
    }
    let out_len = (input.len() as u64 * to as u64 / from as u64) as usize;
    let step = from as f64 / to as f64;
    (0..out_len)
        .map(|i| {
            let x = i as f64 * step;
            let j = x as usize;
            let frac = (x - j as f64) as f32;
            let a = input[j.min(input.len() - 1)];
            let b = input[(j + 1).min(input.len() - 1)];
            a + (b - a) * frac
        })
        .collect()
}
