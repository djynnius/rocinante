---
name: ferraus-vocals
requires: [ferraus]
description: Produce a lead or backing vocal with the ferraus CLI - repair (noise, hum, clicks, clipping, room), pitch correction from natural to hard-tuned, word-by-word volume rides against the music, a measured vocal chain (EQ, compression, de-essing, resonance control, presence), doubles, harmonies and vocal effects - ready to drop into a mix. Use this whenever someone has a raw vocal take, a voice memo, a rap or sung vocal, ad-libs or backing vocals and wants them cleaned up, tuned, "auto-tuned", de-essed, made to sit on a beat, made to sound "radio", "warm", "clear", "airy", or "like a record"; also for telephone, robot, vocoder, or throw-delay vocal effects, and when a vocal is noisy, harsh, sibilant, boomy, or out of tune.
---

# Vocal production with ferraus

The vocal is the most important part of most songs and the one listeners judge hardest. The
job here: make a take clean, in tune, controlled and present, without making it sound
processed (unless that is the brief). You cannot hear it, so every move is justified by a
measurement and checked by one. Hand the result to the `ferraus-mix` skill for placement
in a song.

Run ferraus with `--json`; check flags with `ferraus docs <command>`.

## 1. Intake

```bash
ferraus info vocal.wav --json
ferraus analyze problems vocal.wav --json
ferraus analyze loudness vocal.wav --json
ferraus analyze pitch vocal.wav --json
```

Record: sample rate and channels (vocals are mono sources; a stereo vocal file with
correlation above 0.98 becomes mono with `channels --to mono`), integrated loudness, true
peak, loudness range, and the problem list. From `analyze pitch`: `voiced_fraction`,
`median_note`, range (`min_hz`..`max_hz`), and the per-note `midi` values. Key: from the
producer, else `ferraus analyze key beat.wav` on the instrumental (the beat is a far better
key source than a lone voice); relative major/minor confusions are common.

## 2. Repair, in this order, only what was measured

Each step is a stage in one `fx` chain or pipeline (see Pipeline below), and each is followed
by re-running `analyze problems` to confirm the problem dropped a severity or disappeared.

| Problem | Fix | Notes |
|---|---|---|
| `clipping` | `declip` | before anything else; the repaired peaks need the headroom it adds |
| `dc_offset` | `dc-remove` | |
| `hum` (confirmed in the gaps between phrases) | `dehum` (auto 50/60 Hz) | `--harmonics 8` default; raise for buzzy hum |
| `clicks` (mouth clicks, digital ticks) | `declick --sensitivity 20%` | raise sensitivity carefully: consonants are transients too |
| `noise_floor`, `hiss` (room tone, preamp) | `denoise --noise-range <a noise-only region>` or `denoise --auto` | default 12 dB reduction; keep the floor natural rather than silent |
| a recurring noise (AC, chair, fridge) | `remove-sound --learn <region with only that noise>` | |
| roomy, boxy recording | `dereverb --amount 60%` | heavy settings sound phasey; measure `loudness_range_lu` does not jump |
| plosives (p, b pops) | `enhance-voice` includes a de-plosive; or `highpass --freq 100Hz` | |

Gates: problems at medium severity or above are gone; the noise floor between phrases sits
50 dB or more under the phrase level (`analyze stats`, or `silence detect --threshold -55dB`
finds the gaps); nothing you removed was voice (`analyze pitch` `voiced_fraction` within 2%
of the raw take).

## 3. Tuning

```bash
ferraus tune vocal.clean.wav --key A --scale minor --strength 80% --retune-speed 40ms --humanize 10% --notes-out tune_notes.json -o vocal.tuned.wav --json
```

Read the result's `metrics`: `mean_abs_deviation_cents` (how far the take was from the
grid) and `max_abs_deviation_cents`, plus `tune_notes.json`, which lists every note with
`detected_hz`, `target_note`, `deviation_cents`.

| Style | Settings | When |
|---|---|---|
| transparent | `--strength 50-70% --retune-speed 50-80ms --humanize 10%` | singer-songwriter, acoustic, jazz: fix only what is clearly off |
| modern pop / R&B | `--strength 80-90% --retune-speed 25-40ms` | tight but still human |
| hard tune (T-Pain, trap) | `--effect hard` | the effect is the point; key and scale must be right |
| to a written melody | `--target melody.mid --tolerance 150cents` | the producer has the topline as MIDI |

Rules:

- A note more than about 100 cents from its target is a different note sung (a passing or
  blue note) or a wrong key/scale guess: check the scale before forcing it. `--tolerance`
  with `--target` leaves such notes alone and reports them.
- `--vibrato-preserve on` (default) keeps natural vibrato; never turn it off for natural
  styles.
- `--formant preserve` (default) keeps the voice's character.
- Gate: re-run `analyze pitch` on the result; per-note deviation from the target notes
  should mostly fall within ±15 cents for pop, ±25 for natural styles, and the
  `voiced_fraction` should not drop.

## 4. The vocal chain

**Ride before you compress.** A compressor reacts to the vocal alone. Engineers first draw
volume automation so every word sits against the music, then compress what is left: louder
on a word that drops under the band, softer on one that jumps out. `ride` draws that
automation from measurements:

```bash
ferraus ride vocal.wav --against instrumental.wav -o vocal.ride.wav --json
```

- `--against` is the music the vocal sits in (the instrumental, or every other stem summed,
  starting with the vocal). In a mix pipeline it is the template's `vox_ride` stage, keyed by
  every instrument before vocal-keyed ducking.
- Read `offset_before.spread_lu` → `offset_after.spread_lu`: the p10-p90 spread of
  vocal-minus-music while singing (the mix template's test vocal: 4.9 → 2.5 LU). The ride
  evens out changes slower than about `--window`; faster ones inside a word are the
  compressor's job, which is why it comes first.
- `at_range_share` above 0.2 warns `range_limited`: the vocal is often more than `--range`
  from the music, so the fader or the arrangement is the problem, not the rides.
- `loudness_change_lu`: the ride turns loud words down as far as it turns quiet ones up, so
  the vocal usually ends a fraction of a LU quieter; add it back on the fader when the
  balance must not move.
- `result.points` is the automation as `envelope` keyframes: keep it, or change one word.
- Settings: `--window 400ms` follows words, `1.5s` phrases; `--range 3dB` for a gentle ride,
  `6dB` for an uneven take; `--floor -20dB` leaves loud breaths alone. Without an
  instrumental, `ferraus ride vocal.wav` evens the take against itself.

Starting chain (pipeline `fx` chain items; numbers are starting points, the gates decide):

```yaml
- { op: highpass, freq: 90Hz, order: 2 }                 # 70 Hz deep voices, 100-120 Hz bright voices
- { op: eq, band: ["peak:300Hz:-2dB:q=1.0"] }            # when 200-400 Hz sits 3+ dB above its neighbours
- { op: compress, preset: vocal, threshold: -28dB, makeup: 0dB }   # leveling: 3-6 dB mean reduction
- { op: compress, threshold: -24dB, ratio: 4:1, attack: 2ms, release: 50ms, makeup: 0dB }  # peaks: 1-3 dB
- { op: eq, band: ["peak:3kHz:+2dB:q=1.0", "highshelf:10kHz:+2dB"] }  # presence, air
- { op: deess }                                          # after the boosts that raise sibilance
- { op: saturate, type: tape, drive: 3dB, mix: 20% }     # optional density; saturation depends on the level going in, so check it changed something and remove it if not
- { op: gain, by: 0dB }                                  # the fader (set in the mix)
```

**Static cut or `deresonate`.** A build-up that stays at one frequency (the same resonance
candidate in every phrase) takes the static `eq` cut. One that moves with the notes (the
candidate band changes from phrase to phrase, or the static cut dulls the whole vocal) takes
`deresonate`, which turns a peak down only while it stands out above its neighbourhood:

```yaml
- { op: deresonate, band: 150Hz..800Hz, threshold: 5dB, depth: 3dB }   # after the high-pass, before the compressors
```

Read `result.top` (the sixth-octave bands it worked on, with their mean and largest cut) and
`reduction_mean_db`. Vowel formants count as resonances too: on a test vocal with a 9-10 dB
hump at 500-900 Hz, the defaults (`threshold 3dB`, `depth 6dB`) cut 2.5 dB on average and
4.3 dB at 794 Hz; `threshold 5dB, depth 3dB` cut 1.2 dB and 2.1 dB. Start gentle and go
deeper only while the vocal's 300 Hz region still sits 3 dB or more above 1-2 kHz. Harsh
2-6 kHz on loud notes: `{ op: deresonate, band: 2kHz..6kHz, depth: 3dB }` after the
compressors; sibilance stays the de-esser's job. It is no use below about 150 Hz (too little
frequency resolution) or on a broad, fixed bump (use `eq`).

The quick alternative is `enhance-voice --mode music` (high-pass, de-plosive, de-ess,
compression, presence in one stage) for demos and voice memos.

Measure and gate:

| What | Measure | Target |
|---|---|---|
| leveling | `gain_reduction_mean_db` of the first compressor (`chain_metrics`) | 3-6 dB (sparse takes: judge by max, 6-10 dB) |
| consistency | `analyze loudness --timeline` short-term range across phrases | phrases within ±2 LU of each other |
| against the music | `ride`'s `offset_after.spread_lu` | 3 LU or less |
| loudness range | `loudness_range_lu` | 2-5 LU for pop and rap, up to 7 for ballads and jazz |
| sibilance | `analyze problems`: `sibilance` | gone or low; 5-9 kHz bands no more than 3 dB above 2-4 kHz |
| mud | 200-400 Hz bands vs 1-2 kHz | within 3 dB |
| presence | 2-5 kHz vs the instrumental (in the mix) | see `analyze masking` in the mix skill |
| headroom | true peak | ≤ -6 dBTP on the processed stem leaves room in the mix |

**Verse and chorus that need different treatment.** Takes recorded at another distance or
level (an airy, sibilant verse; a belted chorus) want their own chains: more de-essing and a
roll-off above 10 kHz for the airy one, a different compressor setting for the belted one.
Run both chains on the whole vocal and switch between them at the section boundaries from
`ferraus analyze sections` on the rough mix: an `envelope` at the end of each chain, mixed
together with a `mix` stage. Put each switch inside a gap of the vocal near the boundary
(`analyze silence` on the vocal), with a 20 ms ramp to -90 dB, where nothing is singing.
When a phrase crosses the boundary, crossfade instead. Gain keyframes interpolate in dB, so
mirrored straight ramps would dip by tens of dB in the middle, and the two chains carry the
same vocal, so they add in amplitude: use equal-gain values. Over a 100 ms crossfade
starting at t, the outgoing chain gets 0, -2.5, -6, -12 and -90 dB at t, t+25 ms, t+50 ms,
t+75 ms and t+100 ms, and the incoming chain the same values in reverse order. The sum stays
within 1.6 dB of steady; nine points in 12.5 ms steps (0, -1.2, -2.5, -4.1, -6, -8.5, -12,
-18.1, -90 dB) keep it within 0.7 dB. Equal-power values (0, -0.7, -3, -8.3) would bump the
level 3 dB in the middle.

Voice-type notes: deep male voices need less low cut and more 2-4 kHz; bright female and
child voices need the de-esser lower (`--center 5.5kHz`) and gentler air boost; rap needs
faster leveling (`attack: 5ms`) and more consistency (LRA 2-3 LU).

## 5. Doubles, harmonies, stacks

- **Real doubles** (a second take): process with the lead's chain, 4-6 dB more leveling,
  6-10 dB under the lead, panned ±80-100 or stacked center-low for rap. A double that drifts
  in time is the producer's call; ferraus does not align takes.
- **Artificial double (ADT)**: copy the lead, `pitch --cents 8` (or -8), `delay` with
  `time: 25ms, feedback: 0%, mix: 100%`, darker (`lowpass 8kHz`), 8-12 dB under the lead,
  panned opposite another ADT at `--cents -8`, `time: 18ms`.
- **Harmonies**: a recorded harmony part is best. `pitch --semitones 3` (or 4, 7) with
  `--formant preserve` makes a usable texture for pads and stacks, not a believable lead
  harmony; keep it low and wide.
- **Backing vocal treatment**: more compression (4-8 dB), more de-essing, high-pass 150 Hz,
  darker and wider than the lead (the mix skill's role recipe).

## 6. Effects

| Effect | Stages |
|---|---|
| telephone / radio | `{ op: highpass, freq: 400Hz, order: 4 }`, `{ op: lowpass, freq: 3.4kHz, order: 4 }`, `{ op: saturate, type: hard, drive: 8dB, mix: 40% }` |
| throw delay on a word | a `delay` send with an `envelope` opening the send for that word (`points: "41.2s=-60dB,41.25s=0dB,41.9s=0dB,42s=-60dB"`); throws land on the last word of a phrase, and each region's `start_s` in `analyze silence vocal.wav --threshold -45dB --min 250ms` is a phrase end |
| radio throw | the same send through `highpass 500Hz` + `lowpass 3kHz`, widened with the Haas shift (the mix skill's stereo-placement.md) and a short plate, so the word flies off to the sides |
| hard-tuned robot | `tune --effect hard`; add `chorus --mix 20%` for a wider version |
| vocoder | `vocoder vocal.wav --carrier synth.wav` (or `--midi chords.mid` for an oscillator carrier) |
| lo-fi | `bitcrush`, `lowpass 6kHz`, `saturate type=tape drive=9dB` |
| whisper / ad-lib layer | high-pass 300 Hz, heavy compression, wide reverb send |

## Pipeline

Put repair, tuning and the chain in one pipeline so any step can be changed and re-run
(stages are cached by content):

```yaml
version: 1
inputs:
  vox: vocal_take3.wav
stages:
  - id: clean
    op: fx
    in: vox
    args:
      chain:
        - { op: declip }
        - { op: denoise, auto: true, reduction: 10dB }
        - { op: declick, sensitivity: 20% }
  - id: tuned
    op: tune
    in: clean
    args: { key: A, scale: minor, strength: 80%, retune_speed: 40ms, humanize: 10% }
  - id: chain
    op: fx
    in: tuned
    args:
      chain:
        - { op: highpass, freq: 90Hz }
        - { op: compress, preset: vocal, threshold: -28dB, makeup: 0dB }
        - { op: compress, threshold: -24dB, ratio: 4:1, attack: 2ms, release: 50ms, makeup: 0dB }
        - { op: eq, band: ["peak:3kHz:+2dB:q=1.0", "highshelf:10kHz:+2dB"] }
        - { op: deess }
  - { id: level, op: analyze loudness, in: chain }
  - { id: pitch_after, op: analyze pitch, in: tuned }
outputs:
  - { from: chain, path: vocal.processed.wav, bit_depth: 24 }
  - { from: tuned, path: vocal.tuned.wav, bit_depth: 24 }
```

## Report

`VOCAL_NOTES.md`: problems found and fixed (before → after severity), tuning style and
the numbers (mean and max deviation before and after, notes left alone and why), the chain
with measured gain reduction, LRA and sibilance before → after, and the files delivered.
Next step: the `ferraus-mix` skill with the processed vocal as the lead stem.
