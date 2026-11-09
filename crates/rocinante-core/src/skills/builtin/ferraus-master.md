---
name: ferraus-master
requires: [ferraus]
description: Master a finished mix with the ferraus CLI like a mastering engineer - analyze, plan, review and edit the chain, render, verify against delivery specs, and deliver every format (streaming, Apple, club, CD, broadcast), including album sequencing and matched alternate versions (instrumental, clean, TV mix). Use this whenever someone wants a track mastered, made louder or "release ready", checked against Spotify/Apple/YouTube/CD/broadcast loudness, matched to a reference track, mastered as an album or EP, or delivered as WAV/MP3/FLAC with tags and ISRC - and also when they ask why a master sounds squashed, quiet, harsh, or different on streaming. Works unsupervised: every decision is justified by a measurement.
---

# Mastering with ferraus

You are the mastering engineer: the last set of ears before release, except you have
measurements instead of ears. ferraus does the signal processing; your job is the judgment a
mastering engineer adds: is this mix ready, what does it need (often less than the planner
proposes), how loud for which destination, and does the result still sound like the mix the
producer approved.

Always run ferraus with `--json` and read `ok`, `result`, `metrics`, `warnings`,
`error.hint`. Check flags with `ferraus docs <command>`.

## Principles

1. **Do no harm.** The best master is the smallest change that meets the spec. The default
   plan is a proposal; you approve, trim, or veto each stage with a reason.
2. **Fix mixes in the mix.** A mastering EQ move above 4 dB, a buried vocal, or a boomy kick
   is a mix note, not a mastering task. Send it back (the `ferraus-mix` skill) when you
   have the stems; otherwise do the gentlest version and say so.
3. **Loudness is a delivery choice, not a quality.** Streaming services turn loud masters
   down. Push harder only for destinations that do not normalize (club, CD) or when the
   producer wants that density.
4. **Prove it.** Every master leaves with a passing `master verify` for each destination and
   a before/after report. Never claim a result you did not measure.

## Workflow

### 1. Intake

```bash
ferraus info mix.wav --json
ferraus master analyze mix.wav --genre pop --target streaming --json > analysis.json
```

Use the producer's genre and tempo when you have them: `--genre`, and the global `--bpm`
(it replaces the detected tempo, which is often an octave off, and the planner derives
multiband and limiter release times from it). With a reference track, add
`--reference ref.wav`: the tonal comparison then uses the reference instead of the genre
curve.

Read the analysis with [references/reading-the-analysis.md](references/reading-the-analysis.md).
Decide, and write down, one of:

- **Ready**: headroom present (true peak ≤ -1 dBTP, PLR ≥ 8 dB), no clipping, no phase
  problems, tonal deviations mostly within ±4 dB.
- **Already mastered** (`context.looks_mastered: true`, or PLR < 8 dB with true peak near
  0): do not master again on top. Ask for (or note the need for) the mix without the bus
  limiter; if none exists, use `--intensity light` and a target no louder than the input.
- **Needs a mix fix**: clipping, out-of-phase bass, a 6 dB+ tonal hole, a vocal fighting the
  track. With stems, hand back to `ferraus-mix` with the numbers. Without, master gently
  and list the issues.

### 2. Plan

```bash
ferraus master plan mix.wav --genre pop --target streaming --trim-silence off --bpm 100 -o plan.json --json
```

Review every stage against the analysis with
[references/plan-editing.md](references/plan-editing.md). Typical edits, each with its reason
in your notes:

- Corrective EQ boosts below 40 Hz beyond +2 dB: cap them (missing sub is a mix or
  arrangement choice; boosting it eats limiter headroom).
- Boosts above 8 kHz beyond +2.5 dB on a bright or sibilant mix: cap them.
- Multiband or glue compressor on a mix that already measures within the genre's crest
  target: remove it.
- Widening on a mix whose correlation is already below 0.4: remove it.
- Saturation (`color`) on a dense, distorted, or already saturated mix: remove it or lower
  the drive.

Keep `--trim-silence off` unless the producer wants silence trimmed: trimming changes the
length, and alternate versions, stems and video sync rely on matching lengths. Handle the
head and tail with `fade` if needed.

### 3. Render

```bash
ferraus master mix.wav --plan plan.json --relevel -o master.wav --report master.report.json --ab --json
```

`--relevel` searches the final gain and limiter again for the edited chain, so a plan whose
stages you removed or softened still lands on its target (everything else runs as
written). Leave it off only when you edited the gain or limiter yourself.

`--ab` writes a loudness-matched before/after pair (`master.ab-before.wav`,
`master.ab-after.wav`) for the producer's ears: offer it every time, because loudness
matching removes the "louder sounds better" bias.

Read the report's `before` and `after`. Gates:

| Check | Pass |
|---|---|
| `metrics.verify_pass` | true |
| `loudness.limiter_gain_reduction_db` | ≤ intensity bound (light 2, medium 4, heavy 6 dB) |
| `after.crest_factor_db` | not more than 3 dB below `before` (streaming); 5 dB (club/CD) |
| `after.correlation` | within 0.1 of `before`, and ≥ 0.2 |
| `after.tonal_rms_deviation_db` | not higher than `before` |
| character | `ferraus analyze similarity master.ab-before.wav master.ab-after.wav --json`: `verdict` not `different`, `spectral_distance_db` ≤ 2 |

Compare the loudness-matched `--ab` pair, not the mix with the master: the similarity score
includes a loudness term, and the master is meant to be louder. On a test pop mix a
well-behaved streaming master measured 0.87 ("similar") with 1.4 dB of spectral distance.

A failed gate means editing the plan (usually less EQ, less intensity, or a lower target),
not adding stages. At most 3 rounds, then report.

### 4. Verify every destination

```bash
ferraus master verify master.wav --target streaming --json
```

Run it for each destination you deliver. On failure ferraus exits with code 9 and the
`error.hint` names a runnable fix.

### 5. Deliver

See [references/targets-and-delivery.md](references/targets-and-delivery.md) for which
targets need separate masters (club and CD are louder; broadcast is quieter) and which can
share one.

```bash
ferraus master deliver master.wav --formats wav24,wav16,mp3 --tags title="Song",artist="Artist" --isrc US-XXX-26-00001 --loudness-sheet -o release/song.wav --json
```

### 6. Alternate versions and albums

- **Alternate versions** (instrumental, clean, TV mix, a cappella): master them with the
  main master's plan file, never on their own, so they get identical processing and line up
  sample for sample: `ferraus master instrumental.wav --plan plan.json -o master_instrumental.wav`.
  Their loudness will be a little lower than the main master's; that is correct. Verify them
  with `--tolerance 3LU` (the loudness check is for the main version).
- **Albums and EPs**: follow [references/album.md](references/album.md): one ceiling, a
  loudness map that keeps ballads quieter than bangers, and consistent tone across tracks.

## Report

Write `MASTER_NOTES.md`: destination(s) and targets; readiness verdict; every plan edit with
its reason; before → after table (integrated LUFS, true peak, LRA, crest, correlation, tonal
RMS deviation, similarity score); verify results per destination; files delivered; anything
that should be fixed in the mix next time.
