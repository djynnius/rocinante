---
name: ferraus
requires: [ferraus]
description: Edit, repair, separate, tune, transcribe, notate, mix, and master audio with the ferraus CLI. Use when the user wants to convert or trim audio, apply effects, remove noise, split a song into stems, pitch-correct a vocal, turn humming into MIDI or an instrument part, make sheet music from audio or MIDI, perform a sheet-music PDF, host CLAP/VST3 instruments, mix stems, or master a track to a loudness target. For full jobs, hand off to the companion skills (ferraus-mix, ferraus-master, ferraus-vocals, ferraus-mix-review, ferraus-stems, ferraus-beats).
---

# ferraus

ferraus is installed as the `ferraus` command. It is non-interactive and machine-friendly.

## Always

- Add `--json` and read the envelope: `ok`, `outputs[].path`/`role`, `params`, `metrics`,
  `warnings`, `error.hint`.
- Start with `ferraus info <file> --json` and, for repair work, `ferraus analyze problems <file> --json`.
- Use `--dry-run --json` to preview long or destructive work. Pass `--overwrite` only when
  replacing an output is intended.
- If a command exits with code 5, run the `error.hint` command (or retry with `-y` if the user
  agreed to downloads).
- Find exact flags with `ferraus docs <command>` or `ferraus schema <command>`; list every
  command with `ferraus describe --agent`.

## Companion skills

For a whole job, load the specialist; each one measures every decision and ends with a
pass/fail gate:

| Job | Skill |
|---|---|
| stems → balanced, panned, processed mix (pipeline file + report) | `ferraus-mix` |
| mix → release-ready masters, deliverables, albums, alternate versions | `ferraus-master` |
| raw vocal → repaired, tuned, produced vocal | `ferraus-vocals` |
| feedback on a mix or master, reference comparison, version A/B | `ferraus-mix-review` |
| check, deliver, separate, or remix stems; instrumental/a cappella/TV mixes | `ferraus-stems` |
| drum patterns, chords, bass lines as MIDI; render to audition | `ferraus-beats` |

## Common tasks

| Goal | Command |
| --- | --- |
| Convert / trim / join | `ferraus convert a.mp3 -o a.flac` · `ferraus trim a.wav --range 0:30..1:00` · `ferraus concat a.wav b.wav -o ab.wav` |
| Loudness | `ferraus normalize a.wav --lufs -14` · `ferraus analyze loudness a.wav --json` |
| Effects chain | `ferraus fx in.wav -o out.wav --fx "highpass:freq=80Hz" --fx "compress:preset=vocal" --fx "reverb:mix=18%"` |
| Noise | `ferraus denoise take.wav --auto` · `ferraus denoise take.wav --noise-range 0s..2s` · `ferraus dehum take.wav` · `ferraus declick vinyl.wav --auto` |
| Stems | `ferraus separate song.mp3 --stems 4 -y` (`--vocal-split`, `--drum-split`, `--instrumental`) |
| Pitch | `ferraus tune vocal.wav --key auto` · `ferraus pitch a.wav --semitones -2` · `ferraus stretch a.wav --factor 1.1` |
| MIDI | `ferraus transcribe hum.wav -o part.mid` · `ferraus midi quantize part.mid --grid 1/16` · `ferraus render part.mid --instrument strings` · `ferraus hum idea.wav --to piano` |
| Notation | `ferraus score song.wav --layout lead-sheet -o song.musicxml` · `ferraus score part.mid --solfa only --solfa-txt part.txt` · `ferraus score perform piece.pdf -o piece.wav` |
| Mixing | `ferraus mix balance stems/*.wav --genre pop --json` (faders) · `ferraus analyze masking vocal.wav keys.wav --json` (EQ cut for keys) · `ferraus compress bass.wav --sidechain kick.wav --sidechain-hpf 0Hz --threshold -20dB --ratio 4:1 -o bass_duck.wav` (ducking) · `ferraus mix verify mix.wav --genre pop --json` (ready for mastering?) |
| Mastering | `ferraus master analyze mix.wav --json` → `ferraus master plan mix.wav --bpm 100 -o plan.json` → edit → `ferraus master mix.wav --plan plan.json --relevel -o master.wav` → `ferraus master verify master.wav --target streaming` |
| Check a result | `ferraus compare before.wav after.wav --json` |

## Notes

- Outputs default to `<input>.<command>.<ext>` beside the input; `-o` takes templates
  `{name}`, `{stem}`, `{index}`, `{ext}`.
- Units: `1.5s`, `4b`, `0s..2s`, `-6dB`, `-14LUFS`, `2kHz`, `C4`, `4:1`, `80%`, `1/16`.
- Each output has a `<output>.ferraus.json` sidecar recording exactly what was done.
- Pipeline files (`ferraus pipeline run job.yaml`) cache every stage; `$name` in a stage
  argument refers to an input or another stage's output (a sidechain key, a reference), and
  every stage's measurements come back in `result.plan[].result`.
