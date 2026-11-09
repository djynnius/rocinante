# Revision notes → pipeline edits

Producers give notes in feel words. Each one below maps to the smallest pipeline change that
addresses it, the measurement that should move, and how to report it. Make one change per
note, re-run (cached), re-gate, and report before → after numbers.

| Note | Edit (stage ids from pipeline-template.yaml) | Measure |
|---|---|---|
| "vocal up / down a hair" | `final` gains: a cappella ±0.5..1 dB | V2M moves by the same amount |
| "vocal up a lot" | ±2..3 dB, then re-check masking cuts on music parts (a buried vocal is often masked, not quiet) | V2M, masking scores |
| "vocal sounds harsh / sibilant" | `vox_fx` de-ess `range` +2..4 dB, or reduce the 3 kHz / air boosts | 5-9 kHz bands of the a cappella |
| "vocal sounds muddy / boxy" | `vox_fx` cut `peak:250..400Hz:-2..-3dB:q=1.0` | 200-400 Hz bands of the a cappella |
| "vocal too dry / too wet" | `vox_verb_send` gain ±2..3 dB (and the delay's gain in `vocal_returns`) | vocal space gate |
| "more punch" | snare/kick `transient` attack +1..2 dB; or less bus compression (`mixbus` threshold up 2 dB) | crest factor of the drums bus up 1-2 dB |
| "kick needs more weight" | `kick_fx` `peak:60Hz:+2dB:q=1.0`, and more bass duck so they do not pile up | 50-80 Hz bands, `correlation_below_120hz` |
| "more low end" | bass gain in `instrumental` +1..2 dB (not an EQ boost on the mix bus) | 40-120 Hz vs genre curve in `mix verify` |
| "low end is boomy / muddy" | bass `peak:200..250Hz` cut; kick/bass masking fix; more duck | `mix verify` mud check |
| "too bright / harsh overall" | cut on the brightest bus (usually hats or music), not the mix bus | 2-5 kHz and 6-10 kHz vs genre curve |
| "too dull / dark" | air shelf on vocal and music bus (`highshelf:10kHz:+1..2dB`), hats up 1 dB | 8-16 kHz vs genre curve |
| "wider" | pads/BVs `widen` amount +20%; keep `mono_below` | `correlation` ≥ 0.2, `mono_loss_db` ≤ 3 |
| "narrower / more focused" | lower `widen` on pads/BVs; pull ±80 pans to ±50 | `correlation` up |
| "guitars / keys too loud" | that stem's gain on its bus -1..2 dB | V2M rises slightly |
| "the drop / chorus doesn't hit" | check section contrast; reduce bus compression; widen chorus-only parts | chorus vs verse short-term loudness +1..3 LU |
| "it feels crowded" | push back non-essential parts: -1 dB, darker (lowpass), wetter (+2 dB send) | masking scores, V2M |
| "bass disappears on phone speakers" | add the parallel harmonics return (role-recipes.md, Bass) at -8 dB; raise it toward -6 dB | 700 Hz-1.5 kHz of the bass up 2-4 dB, 50-200 Hz unchanged (`scripts/bands.py --compare`) |
| "make it sound like <reference>" | `mix verify --reference ref.wav`; follow the tonal-balance fixes; then `ferraus-master` with the same reference | tonal deltas shrink |

Applying an edit without opening the file:

```bash
ferraus pipeline run mix/mix.yaml --set stages.final.args.gains=0dB,-1.5dB --overwrite --json   # vocal +1 dB from -2.5 dB
```

For keeps, write the change into `mix.yaml` so the file stays the source of truth.
