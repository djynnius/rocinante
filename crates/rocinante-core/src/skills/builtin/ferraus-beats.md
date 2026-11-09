---
name: ferraus-beats
requires: [ferraus]
description: Program drums, chord progressions, bass lines and melodies as MIDI with the ferraus CLI - genre drum patterns, voiced progressions, grooves and swing, humanized velocities, hummed ideas turned into parts - and render them with soundfonts or CLAP/VST3 instruments to audition, as DAW-ready .mid files and audio stems. Use this whenever a producer asks for a drum pattern, a beat in a genre (trap, boom bap, house, drill, afrobeats, lo-fi, pop), a chord progression, a bass line, an arpeggio, a melody idea, MIDI to drag into their DAW, a groove or swing applied to their MIDI, humanized MIDI, or their humming or beatboxing turned into notes.
---

# Beats and MIDI parts with ferraus

This skill writes parts a producer can drop into a DAW: MIDI first (editable, any
instrument), audio renders second (to audition, or as stems for the `ferraus-mix` skill).
It is a programming assistant, not a song generator: keep parts idiomatic, editable and
short (2-8 bars that loop), name everything clearly, and explain the musical choices.

Run ferraus with `--json`; check flags with `ferraus docs <command>`.

## Workflow

1. **Brief**: genre, tempo, key, feel (straight, swung, half-time), bars, and which parts.
   Default when unspecified: 4 bars, the genre's typical tempo from
   [references/drum-patterns.md](references/drum-patterns.md), a minor key for dark genres.
2. **Write MIDI JSON**, one file per part (drums, bass, chords, lead), in the model
   `ferraus midi from-json` reads (below). Patterns and voicings:
   [references/drum-patterns.md](references/drum-patterns.md) and
   [references/harmony.md](references/harmony.md).
3. **Build and shape**:
   ```bash
   ferraus midi from-json drums.json -o drums.mid --json
   ferraus midi quantize drums.mid --grid 1/16 --swing 56% -o drums_swing.mid --json
   ferraus midi velocity drums_swing.mid --range 70..120 -o drums_final.mid --json
   ```
4. **Render to audition**:
   ```bash
   ferraus render drums_final.mid --map "1=drums" -o drums.wav --json
   ferraus render chords.mid --instrument "Electric Piano 1" --fx "reverb:mix=0.15" -o chords.wav --json
   ferraus render bass.mid --instrument plugin:"Surge XT" -o bass.wav --json    # an installed CLAP/VST3 synth
   ```
   `ferraus sounds list` shows installed soundfonts; `ferraus plugin list --instruments`
   shows plugin synths; `ferraus sounds search piano` finds downloadable sounds.
5. **Check**: `ferraus midi info part.mid --json` (note count, range, duration in bars);
   the rendered parts' loudness and length; that every part is the same number of bars.
6. **Deliver**: the `.mid` files, the rendered `.wav` stems (same length, start at bar 1),
   and a short note: tempo, key, bars, what each part does. For a full arrangement, hand
   the rendered stems to `ferraus-mix`.

## The MIDI JSON model

```json
{
  "ppq": 480,
  "tempo_map": [{"beat": 0, "bpm": 140}],
  "time_signatures": [{"beat": 0, "numerator": 4, "denominator": 4}],
  "tracks": [
    {"name": "drums", "notes": [
      {"beat": 0, "duration_beats": 0.25, "pitch": 36, "velocity": 112, "channel": 10},
      {"beat": 1, "duration_beats": 0.25, "pitch": 38, "velocity": 105, "channel": 10}
    ]}
  ]
}
```

- `beat` counts quarter notes from 0 (bar 2 starts at beat 4 in 4/4); `duration_beats` in
  quarters. `pitch` is a MIDI number, or use `"name": "C#4"` instead (C4 = 60).
- Drums go on `channel` 10 with General MIDI drum numbers (kick 36, snare 38, clap 39,
  closed hat 42, pedal hat 44, open hat 46, low tom 45, mid tom 47, high tom 50, crash 49,
  ride 51, rim 37, shaker 70, cowbell 56).
- Generate long or repetitive parts with a short script rather than typing notes by hand,
  and keep the generator next to the output so the producer can change it.
- Round-trip a producer's MIDI: `ferraus midi to-json their.mid -o their.json` → edit →
  `midi from-json`.

## Feel

| Feel | How |
|---|---|
| swing | `midi quantize --grid 1/16 --swing 54-62%` (hip-hop, house, lo-fi); `1/8` grid for jazz, shuffle |
| humanize timing | `midi quantize --humanize 8ms --seed 7` (reproducible) |
| humanize velocity | vary velocities in the JSON (accents on downbeats, ghost notes 30-60) then `midi velocity --compress 20%` to tame extremes |
| a groove from a real performance | `ferraus midi groove extract their_drums.mid -o groove.json`, then `midi quantize --groove groove.json` |
| half-time | snare on beat 3 only (beat 2 of a half-time bar) |
| triplet hats (trap) | `1/8T` (step 1/3 beat) and `1/16T` rolls |

## From an idea

- Hummed melody → part: `ferraus hum idea.wav --to piano -o idea_piano.wav --json`
  (writes the audio; `ferraus transcribe idea.wav -o idea.mid --quantize 1/16` for the MIDI).
- Beatboxed rhythm → drums: `ferraus transcribe beatbox.wav -o rhythm.mid` gives onsets and
  pitches; map low onsets to kick (36), mid to snare (38), noisy high to hats (42) in the
  JSON and rebuild.
- A loop from a song (for reference, not for release): the `ferraus-stems` skill.

## Quality bar

- Idiomatic first: the reference patterns are what the genre expects; vary them on purpose
  (a fill in bar 4, a dropped kick before a chorus), and say what you varied.
- Bass follows the kick (shared downbeats) and the chord roots unless the genre says
  otherwise (house bass on the off-beat, drill's sliding 808s).
- Voice chords for the instrument: keys in the C3-C5 range, close voicings, smooth voice
  leading (common tones held, other voices move by step); see harmony.md.
- Every part in the key; check `ferraus analyze key part.wav` on the render if unsure.
