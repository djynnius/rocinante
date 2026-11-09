---
name: ferraus-mix
requires: [ferraus]
description: Mix a song from its stems with the ferraus CLI, unsupervised and to a professional standard - gain staging, per-instrument EQ and compression, panning and stereo width, masking fixes and sidechain ducking, reverb and delay sends, bus processing, and a measured pass/fail gate before mastering. Use this whenever someone has stems, multitracks, a stem export, or a folder of parts (kick, bass, vocals, keys...) and wants them mixed, balanced, panned, "made to sound like a record", prepared for mastering, or rebalanced; also when they ask how loud a part should sit, where to pan something, why a mix sounds muddy, thin, harsh, narrow or crowded, or want alternate mixes (instrumental, a cappella, TV mix). Delivers an editable ferraus pipeline file, the mix, bus prints, and a decisions report.
---

# Mixing stems with ferraus

You are the mix engineer. The producer hands you stems and expects a balanced, clean, wide,
punchy mix that is ready for mastering, plus a file they can tweak. You cannot listen, so
every decision is made from a measurement and checked by a measurement. That discipline is
what makes an unsupervised mix trustworthy: the producer can read your report and see *why*
the bass sits where it does, not just that it does.

Run ferraus with `--json` and read the envelope (`ok`, `result`, `metrics`, `warnings`,
`error.hint`). Check flags with `ferraus docs <command>`; never guess one.

## The loop that runs everything

**decide → apply → measure → gate → revise**, at most 3 revisions per gate.

- *Decide* from a number (a band level, a loudness, a masking score), not from a feeling.
- *Apply* by editing the pipeline file, never with one-off commands you cannot replay.
- *Measure* with the analyzer named in the step; most measurements are stages of the
  pipeline itself, so one `pipeline run` returns the mix and its numbers.
- *Gate* against [references/gates.md](references/gates.md).
- *Revise* or move on. Parts interact (a vocal-keyed duck changes when the vocal fader
  moves), so 2-3 rounds are normal. After 3 failed rounds stop pushing: EQ beyond ±6 dB,
  more than 8 dB of gain reduction, or a fader 12 dB off its template mean the problem is in
  the source or the arrangement. Report it with the numbers and move on.

"Sounds good" is never a conclusion in your report. "Vocal-to-music -2.1 LU (pop window
-4..0)" is.

## Deliverables

In a `mix/` folder beside the stems (or where the user says):

1. `mix.yaml`: the session as a pipeline; every gain, EQ, compressor, pan, send and bus is
   an editable stage. Start from [references/pipeline-template.yaml](references/pipeline-template.yaml),
   a complete, tested 10-stem session; keep its structure, change its contents.
2. `mix.wav`: 24-bit at the session rate, true peak ≤ -3 dBTP, no limiter on the bus.
3. `prints/`: instrumental, a cappella, and bus prints from the same run.
4. `MIX_NOTES.md`: decisions and measurements ([references/report-template.md](references/report-template.md)).

Then hand off to the `ferraus-master` skill unless the user only wanted the mix.

## Phase 0 - Brief (do not block on it)

Use what the producer already said: genre, tempo (the DAW tempo beats any detector), key,
a reference track, vibe words, deliverables, and anything that must not change ("the lo-fi
vocal is intentional"). Unsupervised, do not ask: infer, choose the conventional option,
and record the choice in the report so it can be flipped in `mix.yaml`.

Then decide what carries the song before touching a fader. That is usually the lead vocal
plus the main rhythm part: the strummed acoustic of a singer-songwriter song, the drums of a
rock song. Those parts sit on the template. Parts that only decorate (a 12-string shimmer, a
pedal-steel fill) are ear candy and sit a few dB lower. A laid-back acoustic, folk or
country song mixes on `--genre acoustic` (drums back, voice and guitar as equals); a
country-pop song with a full band is `pop`.

## Phase 1 - Intake

For every stem: `ferraus info`, `ferraus analyze loudness`, `ferraus analyze stereo`,
`ferraus analyze problems --min-severity medium` (all with `--json`). Record:

- **Sample rate and length.** One export shares a start time. A length mismatch over 10 ms
  means a stem was exported from a different start: flag it, never guess an offset (the
  `ferraus-stems` skill has the full export check).
- **Silent stems** (integrated below -50 LUFS or `-inf`): leave out and list.
- **Fake stereo**: a stereo file with `correlation` above 0.98 and `width` near 0 holds a
  mono source. To pan it, collapse it first (`{ op: channels, to: mono }`). To make it wide,
  keep both channels and decorrelate them with `chorus` (`mix: 40-50%`); `widen` only scales
  side signal that exists, so on fake stereo it does nothing.
- **Phase-inverted stereo** (`correlation` below 0): `invert-phase --channel right`, or flag.
- **Roles.** `ferraus mix balance` infers them from file names (it also accepts
  `--roles`). Confirm odd ones with `analyze spectrum`. `lead` means the main melodic part
  when nobody sings (a lead synth in an instrumental); a synth line that plays under a vocal
  is `synth`.
- **Tempo and key**: the producer's tempo beats any detector. Otherwise run `analyze tempo`
  on the drum stems with steady hits (hats, the full kit) and compare them: detectors make
  octave errors (100 read as 200), 3:4 errors (100 read as 75), and report low confidence on
  sparse parts; distrust a result below confidence 0.5 or that the stems disagree on.
  `analyze key` on a harmonic stem confuses relative keys (A minor as C major).

- **Song map.** Sum the stems at their starting gains and map the song:
  `ferraus mix balance stems/*.wav --genre pop -o mix/rough.wav --json`, then
  `ferraus analyze sections mix/rough.wav --bpm <tempo> --vocal "<lead vocal stem>" --json`.
  It returns the sections with start and end times, labels for repeats (A, B, A, B, C, B),
  a role guess per section (intro, verse, pre-chorus, chorus, bridge, instrumental, outro),
  loudness per section, and the `repeats` with `offset_s` that the editing pass uses.
  Boundaries are reliable on a steady tempo (pass the DAW tempo: a half- or double-time
  reading halves or doubles the bars; a 2/4 bar or an edit shifts the bar grid after it).
  Roles are heuristics (`role_confidence` 0.5-0.8 with `--vocal`, lower without): the
  producer's arrangement notes beat them. It leans toward splitting: a second verse with a
  much fuller arrangement can get its own letter and the role `section` (a sung `section`
  between choruses is the sign; `--similarity 70%` regroups it). Write the map into the
  report; it drives the per-section vocal check (gates.md) and the section contrast in
  Phase 6.
- **Printed decisions.** Reverb or delay printed on a stem, panning baked into a stereo
  stem, several voices bounced to one track, drums delivered as one stereo sum: these are
  the producer's choices and cannot be undone. Spot them from the file names or the brief
  (wet, fx, verb, BG stack), or from a part whose gaps never fall silent (`analyze silence`
  finds no regions between its phrases). Work around them: no second reverb on a stem that
  is already wet, Haas or chorus to widen a mono stack (stereo-placement.md). List them in
  the report as "would improve with dry or isolated stems".
- **Fix before you mix.** A wrong or squeaky note, a fret slide, a chair creak in one pass
  of a part that repeats: run the editing pass of the `ferraus-stems` skill (`patch` from a
  clean repeat, `repair`, `remove-sound`) before Phase 2, so every later measurement sees the
  fixed part.

`analyze problems` is written for finished audio, so read it with a stem in mind:

| Report on a stem | Meaning | Action |
|---|---|---|
| `silence_head`, `silence_tail` | the part enters late or ends early | **ignore**: trimming would break sync |
| `hum` on any stem | real only if it is there while the part is silent; sustained notes look like hum (G1 is 49 Hz, and a guitar's 200/350 Hz notes read as hum harmonics) | check a stretch before the part enters (`ferraus trim stem.wav --range 0s..8s -o head.wav`, then `analyze problems` or `analyze spectrum` on it); `dehum` only if the hum is there, then confirm the report is gone |
| `loudness_outliers` | quiet intros and breakdowns | ignore; never `leveler` music |
| `clipping` | possibly real; `declip` uses its own detector | `{ op: declip, headroom: 0dB }` in the chain (the default 3 dB headroom moves the level behind the fader's back); remove it if its metrics report `samples_repaired: 0` |
| `dc_offset`, `clicks`, `rumble` | real | the `suggested_fix` (dc-remove, declick, high-pass) |
| `hiss`, `noise_floor` on synths, drums, samples | the noise is part of the sound | ignore; clean only recorded sources (`dehiss`, `denoise`) |
| `sibilance` on a vocal | real | de-ess in the vocal chain (`ferraus-vocals` for heavy cases) |
| `phase`, `out_of_phase_bass` | real | `invert-phase` or `phase-align`, then re-check `analyze stereo` |
| `inter_sample_peaks` | the stem peaks near 0 dBTP | nothing: faders and the float pipeline take care of it |

## Phase 2 - Per-stem chains

One `fx` chain per stem from [references/role-recipes.md](references/role-recipes.md), in
this order: cleanup → high-pass → corrective cuts → compression → tonal boosts →
saturation → de-ess → **pan** (off-centre mono parts) → **fader** (`{ op: gain, by: ... }`,
always last).

- The fader at the end of the chain makes every send taken from that stage post-fader: a
  fader move keeps the part's wet/dry ratio.
- Decide tone moves with `python3 scripts/bands.py <stem>` (region levels and resonance
  candidates under one fixed rule) and check them with `--compare <after>`: a cut that does
  not move its region by at least 1 dB did nothing.
- Compressor presets (`vocal`, `drums`, `bus`) default to auto makeup gain. Set
  `makeup: 0dB` on every compressor so all level decisions live in the faders.
- Read gain reduction from the run: `result.plan[].result.chain_metrics` (per chain
  element) or `result.metrics` carry `gain_reduction_mean_db` and `gain_reduction_max_db`.
  Move `threshold` first to reach the role's range.
- Cuts before boosts, narrow cuts and wide boosts, no move beyond ±6 dB, and a move that
  does not change the measurement it was made for comes out again.
- A resonance that moves with the notes (a vocal's low mids, a guitar's ring) gets
  `deresonate` instead of a fixed cut; read its `result.top` to see what it cut. Gentle
  settings and measured amounts are in role-recipes.md.

## Phase 3 - Balance

1. Starting faders: `ferraus mix balance stems/*.wav --genre pop --anchor-lufs -21LUFS --json`.
   Read each input's `role`: roles come from file names, and a wrong one (a "Synth Lead"
   that plays under the vocal is `synth`, not `lead`) changes its target by several dB.
   Re-run with `--roles` (one per stem, `auto` to keep a guess) until every role is right.
   Its `gains` put every raw stem on the genre template (lead vocal anchor, other roles at
   fixed offsets). Put them in the faders.
2. The pipeline's `balance` stage runs `mix balance` on the processed stems. Its gains are
   now corrections: add each to that stem's fader and re-run until all are within ±0.5 dB.
   Processing changes levels (a compressor without makeup is quieter, an EQ boost on a kick
   is louder), so this is the step that makes the template true for what you hear. A
   deliberate departure (the brief asks for a heavier kick) goes into the report with its
   dB; that stem's gate is then "within ±0.5 dB of the departure".
3. Set the vocal-to-music balance (V2M) with the a cappella gain of the `final` stage, to
   the centre of the genre window in gates.md. The template tends to leave the vocal 1-3 dB
   hot; V2M is the authority.
4. V2M is an average; the template's `vox_ride` stage holds it word by word, riding the lead
   vocal against every instrument before its compressors (what an engineer's volume
   automation does). Gate: its `offset_after.spread_lu` ≤ 3 LU. The settings and how to read
   the result are in the `ferraus-vocals` skill (section 4).

## Phase 4 - Stereo placement

Place every source with [references/stereo-placement.md](references/stereo-placement.md):
low end and lead vocal centred, similar ranges mirrored, wide parts widened with mono bass.
Pan off-centre mono stems inside their chain (`{ op: pan, position: -45 }` before the
fader), and give the bus `mix` stages no `pans`: `mix` applies a -3 dB pan law to every mono
input once `pans` is given, so those parts would play 3 dB under what the `balance` stage
measured. Stereo stems keep their image (`widen`, `center_pan`, `pan --mode balance`).
Gate on the `mix_stereo` stage: `|balance_db|` ≤ 1 (positive means the left side is
louder), `correlation` ≥ 0.2, `correlation_below_120hz` ≥ 0.9, `mono_loss_db` ≤ 3. A mix
built mostly from mono sources measures around 0.9 correlation even when well placed; when
the brief asks for width, add side signal (chorus, doubles, wider returns: the width toolkit
in stereo-placement.md) rather than panning further out.

## Phase 5 - Masking and ducking

The pipeline's `mask_*` stages run `analyze masking <priority> <yielding>` on post-fader
stems: kick vs bass, and lead vocal vs every midrange part that plays with it. The score is
level-aware: a band counts only while the yielding part is within 10 dB of the priority part
there, so cuts and fader moves lower it. A pair passes when it reports no `regions`.

For each region: apply its `suggested_fix` (an `eq` cut sized to clear the threshold) to the
yielding stem's chain, before the compressor, then re-run. One cut per region, then
re-measure; do not stack cuts. Skip pairs with `co_activity` below 0.2. On raw, unbalanced
stems use `--levels ignore` (shape only) to predict clashes.

When a cut would hollow out the yielding part, duck it with an external sidechain instead,
so it only gives way while the priority part plays:

```yaml
- id: bass_duck
  op: compress
  in: bass_fx
  args: { sidechain: $kick_fx, sidechain_hpf: 0Hz, threshold: -20dB, ratio: 4:1, attack: 1ms, release: 90ms, makeup: 0dB }
```

`$kick_fx` refers to another stage's output (`$name` to an input); the pipeline orders and
caches it like `in:`. Keyed by a kick or bass, set `sidechain_hpf: 0Hz` (the 80 Hz default
would hide the key). Set duck thresholds after the key's fader is final: start about 12 dB
under the key's post-fader true peak and move in 2 dB steps until the stage's
`gain_reduction_max_db` is in range. Amounts per use are in
[references/space-and-movement.md](references/space-and-movement.md).

## Phase 6 - Space and depth

Shared send buses, not a reverb per stem: a `mix` stage of the post-fader stems at send
levels, then `reverb` or `delay` at `mix: 100%`. Keep vocal returns and instrument returns
separate (`vocal_returns`, `inst_verb`) so the instrumental and the a cappella stay correct.
Duck the vocal reverb and delay under the dry vocal. Depth: the lead vocal is the driest and
brightest element; pads and BVs the wettest and darkest. Gate per family, each against its
own dry bus so the `final` gains cancel out: for pop, vocal returns 10-18 LU under the dry
vocal bus, instrument returns 14-24 LU under the dry instrumental
([references/gates.md](references/gates.md)). Tempo math and starting values:
[references/space-and-movement.md](references/space-and-movement.md).

Section contrast: from the song map, verses can stay intimate (the vocal delay or echo send
down or off, a drier vocal) and choruses open up (echo and plate up, BVs wider). That is an
`envelope` on the send's `mix` stage with keyframes 100-200 ms either side of each section
boundary (`points: "0s=-60dB,28.6s=-60dB,28.8s=0dB,47.9s=0dB,48.1s=-60dB"`). It is a creative
convention: do it when the brief or the genre calls for movement, and name the stage in the
report so the producer can flatten it.

## Phase 7 - Mix bus and the final gate

Mix bus: gentle glue (`compress` `preset: bus`, `makeup: 0dB`, threshold set for 0.5-2 dB
mean reduction), then a trim so true peak ≤ -3 dBTP. No limiter: mastering needs the
headroom. Tape or console saturation on the buses and the mix bus is optional color, with
measured costs (PLR, a darker top) in role-recipes.md (Bus color). A dull top end against
the reference wants a 10-12 kHz shelf, not 7.5 kHz (role-recipes.md, Top end).

```bash
ferraus mix verify mix/mix.wav --genre pop --json
```

`mix verify` checks headroom, loudness window, PLR, clipping, DC, phase, mono bass, mono loss
and tonal balance against the genre curve (or `--reference ref.wav`), with a runnable `fix`
per failed check. It names only the worst tonal band; for the whole curve, read
`tonal_balance.bands[].delta_db` from `ferraus master analyze mix/mix.wav --genre pop --json`
(a "solid low end" brief means checking 40-120 Hz there, not just passing the 6 dB
tolerance). With several references (3-5 tracks the producer named, or that share the
genre and arrangement), run the check against each and act only on deviations they agree on:
a band off from all of them in the same direction. A low end that is weak because the bass part has no energy below 55 Hz is fixed
in the parts (a heavier kick or sub, the brief's call), not by boosting empty bands. A mix
leaves this skill when `mix verify` passes, or when the report says, with numbers, which
check was left failing and why.

Last, a level-matched A/B against the rough mix: the raw stems at their starting gains, no
processing (`ferraus mix balance stems/*.wav --genre pop -o mix/rough.wav --json` writes it).
Louder reads as better, so match first:
`ferraus normalize mix/rough.wav --lufs <the mix's integrated LUFS> -o mix/rough_matched.wav`, then
compare `analyze spectrum` and `analyze stereo` of the two. Every difference should trace to
a decision in the report; put the main ones in it (low end, presence, top, width).

## Phase 8 - Deliver

1. `ferraus pipeline run mix/mix.yaml --bpm <tempo> --overwrite --json` (cached: only
   changed stages recompute).
2. The template's outputs write the mix, the instrumental and a cappella, and the bus prints.
   A TV mix is the instrumental plus BVs: one more `mix` stage and output.
3. Write `MIX_NOTES.md`.
4. Hand off: the `ferraus-master` skill, or
   `ferraus master mix/mix.wav --target streaming --genre <genre> -o master.wav`.

## Disk use

Every stage result is a 32-bit float WAV in the pipeline cache (`ferraus doctor` shows where;
`FERRAUS_HOME` moves it): a full run of a 10-stem, 80-second session writes about 0.75 GB,
and every changed stage adds more. When a session is done, or the disk is tight,
`ferraus pipeline prune --max-size 2GB` (or `--days 0`) clears what is no longer needed.

## Revision notes

Producers answer with "vocal up a hair" or "more punch". Map each note to the smallest
`mix.yaml` change with [references/revision-notes.md](references/revision-notes.md), re-run,
re-gate, and report before → after. `--set` edits one value for a quick try (here the vocal
goes up 0.5 dB from the template's -2.5 dB):

```bash
ferraus pipeline run mix/mix.yaml --set stages.final.args.gains=0dB,-2dB --overwrite --json
```

Write accepted changes into `mix.yaml` so the file stays the source of truth.

## Honesty rules

- Report measurements, not adjectives. Where a choice could only be judged by ear (the tone
  of a distorted guitar, whether an effect is tasteful), say it is a convention and name the
  stage that holds it.
- Never claim a gate passed without the output that shows it.
- Leave the producer's creative choices alone unless they measurably break a gate.
