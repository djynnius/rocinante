//! Hear `/talk`'s voice (Phonon) read some text through the speakers:
//!
//!   cargo run --release -p rocinante-voice --example speak -- "Hello, Donald."
//!
//! Downloads the Phonon files into ~/.rocinante/voice on first run. The text
//! goes through the same markdown-to-sentences step as replies do.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use rocinante_voice::models;
use rocinante_voice::speech_text::Splitter;
use rocinante_voice::{Speaker, SpeakerEvent};

#[tokio::main]
async fn main() {
    let text = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    let text = if text.is_empty() {
        "Hi Donald. I've put the fix in main.rs, and the tests pass. Want me to commit it?".into()
    } else {
        text
    };
    let speakers = rocinante_voice::find_speakers().unwrap_or_else(|e| panic!("no speakers: {e}"));
    let dir = models::dir().expect("home directory");
    for file in models::PHONON {
        models::fetch(&dir, &file, |_, _| {})
            .await
            .expect("download");
    }
    let t0 = Instant::now();
    let (tx, rx) = mpsc::channel();
    let speaker = Speaker::start(&dir.join(models::PHONON_DIR), &speakers, move |e| {
        let _ = tx.send(e);
    })
    .expect("start");
    println!("{}: voice loaded in {:?}", speakers.name, t0.elapsed());

    let mut splitter = Splitter::new();
    let mut sentences = splitter.push(&text);
    sentences.extend(splitter.finish());
    let t0 = Instant::now();
    for s in sentences {
        println!("  say: {s}");
        speaker.say(s);
    }
    loop {
        match rx.recv_timeout(Duration::from_secs(60)) {
            Ok(SpeakerEvent::Speaking) => println!("{:?}: speaking", t0.elapsed()),
            Ok(SpeakerEvent::Done) => {
                println!("{:?}: done", t0.elapsed());
                break;
            }
            Err(_) => panic!("no Done within 60 s"),
        }
    }
}
