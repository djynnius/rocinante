# Role recipes

Starting chains per role, written as pipeline `fx` chain items. Every move has a **when**:
the measurement that justifies it. Skip a move whose condition is not met. Numbers are
conventional starting points from mixing practice, not laws; the gates decide.

How to measure the conditions:

- Band energy: `python3 scripts/bands.py <stem>` (in this skill) prints region levels (sub,
  low, low-mid, mud, mid, presence, brilliance, air) and resonance candidates under one
  fixed rule: a third-octave band at least 6 dB above the median of the three bands on each
  side, counting only bands within 30 dB of the stem's loudest band and never at the
  spectrum's edges. "Boxy", "ring", "honky" below mean a resonance candidate in that range.
  A band that only looks loud because the stem has nothing below it (a vocal's 250 Hz) is
  not a candidate. `--compare <after>` shows what a move changed.
- Mix-level triggers come first: `mix verify`'s `mud`, `harsh` and `sub_rumble` checks and
  `analyze masking` regions say which band of the mix is the problem; then cut the stems
  that carry the most of it, post-fader.
- Dynamics: `ferraus analyze loudness <stem> --json` (`loudness_range_lu`, `true_peak_dbtp`)
  and `ferraus analyze stats <stem> --json` (crest factor).
- Gain reduction: the stage's `result.metrics` / `result.chain_metrics` in the
  `pipeline run` output. `gain_reduction_mean_db` averages over silent passages too, so for
  a sparse part (one that plays less than 60% of the song: `a_active` in `analyze masking`,
  or `silence detect`) use the "sparse max" column of the compression targets instead.

Order inside a chain: cleanup → high-pass → corrective cuts → compression → tonal boosts →
saturation → de-ess → fader (`{ op: gain, by: <dB> }`, always last; see the SKILL).

Every compressor below sets `makeup: 0dB`. The `vocal`, `drums` and `bus` presets default
to auto makeup (the vocal preset adds about 6-7 dB), which silently moves the balance; keep
level decisions in the faders.

## Contents

- [Kick](#kick) · [Snare](#snare) · [Hats and cymbals](#hats-and-cymbals) ·
  [Percussion](#percussion) · [Drum bus or full kit](#drum-bus-or-full-kit)
- [Bass](#bass) · [Sub and 808](#sub-and-808)
- [Lead vocal](#lead-vocal) · [Backing vocals](#backing-vocals)
- [Electric and acoustic guitar](#electric-and-acoustic-guitar) · [Keys and piano](#keys-and-piano)
- [Pads and strings](#pads-and-strings) · [Solo instruments](#solo-instruments-harmonica-fiddle-sax-slide-lead-acoustic) ·
  [Synth leads, arps, plucks](#synth-leads-arps-plucks)
- [Brass](#brass) · [FX, risers, impacts](#fx-risers-impacts)
- [Bus color](#bus-color-tape-and-console) · [Top end against a reference](#top-end-against-a-reference)
- [High-pass table](#high-pass-table) · [Compression targets](#compression-targets)

## Kick

```yaml
- { op: declip, headroom: 0dB }                   # only when analyze problems reports clipping; remove if it repairs 0 samples
- { op: highpass, freq: 30Hz, order: 4 }          # when: energy below 30 Hz within 12 dB of the 50-80 Hz peak
- { op: eq, band: ["peak:350Hz:-3dB:q=1.4"] }      # when: bands.py finds a resonance candidate at 250-500 Hz (boxy); cut at that band
- { op: transient, attack: +3dB }                 # when: crest factor below 12 dB (the hit lacks snap)
- { op: compress, threshold: -18dB, ratio: 4:1, attack: 10ms, release: 80ms, makeup: 0dB }
- { op: eq, band: ["peak:60Hz:+2dB:q=1.0", "peak:4kHz:+2dB:q=1.2"] }  # weight / beater click, only if lacking vs the genre curve
```

Kick and bass share the bottom octave: decide which one owns the fundamental (usually the
kick around 50-60 Hz in pop/rock, the 808 or bass below 50 Hz in hip-hop/EDM) and cut the
other there (`analyze masking kick.wav bass.wav`).

## Snare

```yaml
- { op: highpass, freq: 80Hz }
- { op: eq, band: ["peak:900Hz:-2dB:q=1.4"] }      # when: a resonance candidate at 700 Hz-1.2 kHz (ring); cut at that band
- { op: transient, attack: +2dB }
- { op: compress, preset: drums, makeup: 0dB }     # -16 dB, 4:1, 15 ms attack lets the crack through
- { op: eq, band: ["peak:200Hz:+2dB:q=1.2", "highshelf:5kHz:+2dB"] }  # body / crack, when thin or dull vs the drum bus
```

## Hats and cymbals

```yaml
- { op: highpass, freq: 350Hz, order: 4 }          # hats carry nothing useful lower
- { op: eq, band: ["peak:8kHz:-2dB:q=2.0"] }       # when: 6-10 kHz band is the loudest band of the whole mix (harsh)
- { op: transient, sustain: -2dB }                 # when: the hats wash (loudness range of the stem below 4 LU)
```

Hats are usually 10-15 dB under the lead vocal; they read as "too loud" long before they
measure loud, so err low and check the mix's 6-10 kHz bands against the genre curve.

## Percussion

```yaml
- { op: highpass, freq: 200Hz }
- { op: transient, attack: +2dB }
```

Pan percussion opposite the hats so the top end balances. When the shaker or tambourine is
the only part carrying the subdivision (drums delivered as one stereo sum), keep it near
centre instead (stereo-placement.md, rule 9).

- Splashy tambourine or shaker (its 8-16 kHz region louder than the cymbals' in `bands.py`):
  `{ op: lowpass, freq: 10kHz }` or `highshelf:8kHz:-3dB`.
- A bone-dry shaker or tambourine against drums that carry their own room: a room-reverb send
  at -14..-18 dB, so it plays in the same space.

## Drum bus or full kit

When drums arrive as one stem (or as an overheads/room pair), treat the stem as the bus:

```yaml
- { op: highpass, freq: 30Hz, order: 4 }
- { op: compress, ratio: 4:1, threshold: -18dB, attack: 20ms, release: 120ms, makeup: 0dB }   # glue: 1-3 dB mean
- { op: compress, ratio: 8:1, threshold: -30dB, attack: 1ms, release: 60ms, mix: 25% }        # parallel (New York) punch
- { op: saturate, type: tape, drive: 3dB, mix: 30% }                                        # density, when crest factor > 18 dB
```

## Bass

```yaml
- { op: highpass, freq: 30Hz, order: 4 }
- { op: eq, band: ["peak:250Hz:-3dB:q=1.2"] }      # when: masking with kick or keys at 200-315 Hz
- { op: compress, threshold: -22dB, ratio: 4:1, attack: 15ms, release: 120ms, makeup: 0dB }  # 3-6 dB mean: a steady bass is a big part of a solid low end
```

Then duck it under the kick when they hit together (`compress` with `sidechain: $kick_fx`,
2-4 dB). Bass is always centred and mono below 120 Hz.

**Harmonics for small speakers** (when the bass's 700 Hz-1.5 kHz region sits more than 25 dB
under its low region, or the brief says it disappears on phones): a parallel, band-limited
distortion return, so the fundamental is untouched. In-line saturation on a stem that peaks
well under 0 dBFS barely reaches the nonlinearity (6 dB of tube drive at 25% measured +0 dB
at 700 Hz-1.5 kHz on the test bass), and pushing it until it does also dulls the low end.

```yaml
- id: bass_harm
  op: fx
  in: bass_fx
  args:
    chain:
      - { op: saturate, type: hard, drive: 18dB, mix: 100% }
      - { op: highpass, freq: 500Hz, order: 4 }
      - { op: lowpass, freq: 1.8kHz, order: 4 }
- id: bass_full
  op: mix
  in: [bass_fx, bass_harm]
  args: { gains: [0dB, -8dB] }
```

Measured on the test bass: return at -8 dB → 700 Hz-1.5 kHz +3.2 dB, 2-4 kHz +2.3 dB, 50-200
Hz unchanged; -6 dB → +4.2 / +3.3; -10 dB → +2.4 / +1.5. Feed `bass_full` (not `bass_fx`) to
the duck and the bus.

## Sub and 808

```yaml
- { op: channels, to: mono }
- { op: highpass, freq: 25Hz, order: 4 }
- { op: compress, threshold: -20dB, ratio: 3:1, attack: 20ms, release: 150ms, makeup: 0dB }
```

For audible upper harmonics, use the bass harmonics return above (return at -10..-6 dB).

In hip-hop and EDM the 808/sub usually owns 40-60 Hz and the kick is cut there instead (or
ducks it, the other way round). Check `correlation_below_120hz` stays above 0.9.

## Lead vocal

Tuning, comping and repair belong to the `ferraus-vocals` skill; run it first if the vocal
is raw. In the mix:

```yaml
- { op: highpass, freq: 90Hz, order: 2 }           # 70 Hz for deep male voices, 100-120 Hz for bright female voices
- { op: eq, band: ["peak:300Hz:-2dB:q=1.0"] }      # when: mix verify warns `mud`, or masking flags 200-400 Hz, or a resonance candidate there
- { op: deresonate, band: 150Hz..800Hz, threshold: 5dB, depth: 3dB }  # instead of the cut when the build-up moves with the notes (ferraus-vocals, section 4)
- { op: compress, preset: vocal, threshold: -28dB, makeup: 0dB }  # leveling: 3-6 dB mean; set threshold from the measured reduction
- { op: compress, threshold: -24dB, ratio: 4:1, attack: 2ms, release: 50ms, makeup: 0dB }  # peaks: 1-3 dB max
- { op: eq, band: ["peak:3kHz:+2dB:q=1.0", "highshelf:10kHz:+2dB"] }  # presence and air, when the vocal is masked or dull
- { op: deess }                                    # after the boosts, which raise sibilance
```

The lead vocal is ridden against the instruments before this chain (the template's
`vox_ride` stage; the `ferraus-vocals` skill explains the numbers). Use the `eq` cut or
`deresonate`, not both, unless the measurements call for each.

The vocal is the reference point of the mix: centred, driest, brightest, and inside the
vocal-to-music window (gates.md). Everything that competes with it between 1 and 5 kHz
yields (masking cuts or a gentle sidechain duck).

## Backing vocals

```yaml
- { op: highpass, freq: 150Hz }
- { op: eq, band: ["peak:250Hz:-3dB:q=1.0", "peak:3kHz:-2dB:q=1.0"] }  # get out of the lead's way
- { op: compress, threshold: -24dB, ratio: 4:1, attack: 5ms, release: 80ms, makeup: 0dB }  # 4-8 dB: BVs sit best when flat
- { op: deess, range: 8dB }
- { op: lowpass, freq: 12kHz }                     # darker than the lead pushes them back
```

Wider (`widen` 130-160% on stereo BV stems, or pairs panned ±40-80) and wetter than the lead.

## Electric and acoustic guitar

```yaml
- { op: highpass, freq: 90Hz }                     # 120 Hz for acoustic strumming in a full band
- { op: eq, band: ["peak:300Hz:-2dB:q=1.2"] }      # when: masking with bass/keys in 200-400 Hz
- { op: compress, threshold: -20dB, ratio: 2:1, attack: 20ms, release: 150ms, makeup: 0dB }  # 1-3 dB; acoustic strumming up to 4 dB
- { op: eq, band: ["peak:3kHz:-2dB:q=1.0"] }       # when: analyze masking vocal vs guitar flags 2-4 kHz
```

Distorted guitars are already compressed; skip the compressor unless the stem's loudness
range is above 6 LU. A guitar that rings at 2-5 kHz on some notes (a resonance candidate that
comes and goes): `{ op: deresonate, band: 2kHz..6kHz, depth: 3dB }` rather than a fixed cut;
on a test guitar with a +9 dB ring added at 3 kHz it found 2.8 and 3.2 kHz and cut that band
by 2 dB. A broad, fixed bump is still an `eq` job.

## Keys and piano

```yaml
- { op: highpass, freq: 70Hz }                     # 40 Hz for a solo-piano arrangement where the piano is the bass
- { op: eq, band: ["peak:400Hz:-2dB:q=1.0"] }      # when: masking with guitar/bass
- { op: compress, threshold: -22dB, ratio: 2:1, attack: 20ms, release: 150ms, makeup: 0dB }  # 1-3 dB
```

## Pads and strings

```yaml
- { op: highpass, freq: 180Hz }                    # pads add width and air, not low end
- { op: lowpass, freq: 12kHz }                     # when: 10-16 kHz louder than the vocal there
- { op: chorus, mix: 50% }                         # when: fake stereo (correlation > 0.98); keep the file 2-channel
- { op: widen, amount: 130%, mono_below: 200Hz }
```

Duck pads 1-3 dB under the lead vocal (`sidechain: $vox_fx`) when masking reports a
co-active region in 1-4 kHz, or under the kick for an EDM pump (see space-and-movement.md).

## Solo instruments (harmonica, fiddle, sax, slide, lead acoustic)

Players shape these phrases with their dynamics; a compressor flattens what they built.

```yaml
- { op: highpass, freq: 120Hz }
- { op: limit, ceiling: -9dBTP, release: 50ms }    # catch one or two blasts: ceiling about 3 dB under the stem's true peak
```

When: `analyze loudness --timeline` shows one or two momentary spikes 6 dB or more above the
rest of the part. Set `ceiling` about 3 dB under the stem's `true_peak_dbtp` and check the
chain metrics: a few dB at most, almost nothing on average. For a single blast an `envelope`
dip on that note is more transparent still. Give a part's first entrance its level (an
`envelope` +1..+2 dB on its first phrase) so the handoff from the vocal lands, and send it to
the vocal plate so the two share a space.

## Synth leads, arps, plucks

```yaml
- { op: highpass, freq: 150Hz }
- { op: eq, band: ["peak:2.5kHz:-2dB:q=1.0"] }     # when: co-active with the vocal in 2-4 kHz
- { op: compress, threshold: -20dB, ratio: 2:1, attack: 10ms, release: 100ms, makeup: 0dB }
```

A lead synth that plays a melody *while* the vocal sings is an arrangement clash: duck it
(3-4 dB keyed by the vocal) and note it in the report; between vocal phrases it can be loud.

## Brass

```yaml
- { op: highpass, freq: 100Hz }
- { op: eq, band: ["peak:1.2kHz:-2dB:q=1.0"] }     # when: a resonance candidate at 800 Hz-1.5 kHz (honky)
- { op: compress, threshold: -20dB, ratio: 3:1, attack: 15ms, release: 120ms, makeup: 0dB }
```

## FX, risers, impacts

```yaml
- { op: highpass, freq: 200Hz }                    # impacts that are meant to hit keep their lows: 40 Hz
```

Usually 12-18 dB under the vocal; wide, wet, and allowed to move (autopan, pan sweeps).

## Bus color: tape and console

Saturation on the buses and the mix bus is a habit of many engineers ("everything through
tape and a console"). It is a convention, not a fix: add it when the brief asks for warmth,
glue or density, or the mix measures sterile (PLR above 16 dB on a pop mix), and never to
win an A/B by being louder. Measured on a test mix at matched loudness:

| Stage | PLR | Tone (level-matched) |
|---|---|---|
| `{ op: saturate, type: tape, drive: 3dB }` | -3.3 dB (peaks rounded off: true peak -3.4 → -7.1 dBTP) | darker top: -0.7 dB at 8 kHz, -1.4 dB at 12.7 kHz, -1.8 dB at 16 kHz |
| `{ op: saturate, type: soft, drive: 3dB }` (console-like) | -2.0 dB | unchanged |
| `{ op: saturate, type: tube, drive: 3dB }` | -0.3 dB | unchanged |
| `type: tape, drive: 6dB` | -4.8 dB | as 3 dB |

Saturation responds to the level going in, so measure after the faders are set. Stacking it on
every bus and the mix bus adds up: check `mix verify`'s PLR (≥ 10 dB) after the whole chain,
and re-run the `balance` stage, because density changes how loud parts read. Tape darkens: if
the brief also wants sparkle, follow it with the top-end shelf below rather than driving
harder.

## Top end against a reference

When a reference or the genre curve says the mix is dull above 8 kHz, use a high shelf at
10-12 kHz, not 7.5 kHz: the lower shelf also lifts 5-8 kHz, where harshness lives. Measured
on a test mix:

| Shelf | 5 kHz | 8 kHz | 12.7 kHz | 16 kHz |
|---|---|---|---|---|
| `highshelf:7.5kHz:+3dB` | +0.4 | +1.7 | +2.8 | +3.0 |
| `highshelf:10kHz:+2dB` | +0.1 | +0.5 | +1.6 | +1.9 |
| `highshelf:12kHz:+3dB` | +0.0 | +0.3 | +1.8 | +2.6 |

If the brighter mix turns harsh (`mix verify`'s `harsh` check, or 2-6 kHz rising above the
reference), tame the peaks that ring rather than the whole band:
`{ op: deresonate, band: 2kHz..6kHz, depth: 3dB }` on the bus that carries them. ferraus has no
exciter; a parallel distortion of the top band, built from `highpass` + `saturate` + `mix`,
measured poorly (it partly cancels the band it overlaps), so prefer the shelf.

## High-pass table

| Role | Cutoff | Order |
|---|---|---|
| kick | 30 Hz (only if rumble) | 4 |
| snare | 80 Hz | 2 |
| hats, cymbals | 300-500 Hz | 4 |
| percussion | 150-250 Hz | 2 |
| bass | 30 Hz | 4 |
| sub, 808 | 25 Hz | 4 |
| lead vocal | 70-120 Hz | 2 |
| backing vocals | 120-180 Hz | 2 |
| guitars | 80-120 Hz | 2 |
| keys, piano | 50-80 Hz | 2 |
| pads, strings | 150-250 Hz | 2 |
| synth lead, arp | 120-200 Hz | 2 |
| brass | 90-120 Hz | 2 |
| fx | 150-250 Hz | 2 |

## Compression targets

| Stage | Mean gain reduction (continuous parts) | Max, never above | Sparse max (parts playing < 60% of the song) |
|---|---|---|---|
| lead vocal (leveling) | 3-6 dB | 10 dB | 6-10 dB |
| lead vocal (peaks) | 0.5-2 dB | 4 dB | 1-3 dB |
| backing vocals | 4-8 dB | 12 dB | 6-12 dB |
| kick | 1-3 dB | 6 dB | 3-6 dB |
| snare | 2-4 dB | 8 dB | 3-6 dB |
| drum bus (glue) | 1-3 dB | 6 dB | 2-5 dB |
| bass | 3-6 dB | 10 dB | 4-8 dB |
| guitars, keys | 1-3 dB | 6 dB | 2-5 dB |
| pads | 0-2 dB | 4 dB | 1-3 dB |
| mix bus | 0.5-2 dB | 4 dB | n/a |

Outside the range: move `threshold` first (1 dB of threshold is roughly 1 dB of reduction at
4:1 once the signal is well over it), and `ratio` only when the character is wrong.
