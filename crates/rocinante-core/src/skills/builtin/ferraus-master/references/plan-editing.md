# Reviewing and editing a master plan

`ferraus master plan mix.wav ... -o plan.json` writes the exact chain `master` would run,
every parameter resolved. The file is JSON:

```json
{
  "version": 1,
  "target": {"name": "streaming", "lufs": -14.0, "true_peak_dbtp": -1.0},
  "genre": {"name": "pop", "source": "explicit"},
  "intensity": {"name": "medium", "limiter_max_gain_reduction_db": 4.0},
  "stages": [
    {"stage": "preflight", "op": "dc-remove", "args": {"freq": "5Hz"}, "why": "..."},
    {"stage": "corrective_eq", "op": "eq", "args": {"band": ["peak:25Hz:+3.9dB:q=4.32", "..."]}, "why": "..."},
    {"stage": "dynamics", "op": "multiband", "args": {"...": "..."}, "why": "..."},
    {"stage": "loudness", "op": "gain", "args": {"by": "+7.06dB"}, "why": "gain to the -14.0 LUFS target"},
    {"stage": "loudness", "op": "limit", "args": {"ceiling": "-1dBTP", "...": "..."}, "why": "..."}
  ],
  "expected": {"integrated_lufs": -14.0, "true_peak_dbtp": -1.05, "limiter_gain_reduction_db": 0.02}
}
```

Each stage's `op` and `args` are an ordinary ferraus effect with its flags (underscores
for dashes). Edit values, delete stages, or add stages;
`ferraus master mix.wav --plan plan.json` runs exactly what the file says.
`ferraus docs <op>` documents every argument.

## Stage-by-stage review

| Stage | Op | Keep when | Typical edit |
|---|---|---|---|
| preflight | `dc-remove` | always (harmless) | - |
| preflight | `silence trim` | the producer wants trimmed edges | plan with `--trim-silence off` to omit it (alternate versions must match lengths) |
| preflight | `declip` | the analysis found clipping | - |
| corrective_eq | `eq` (1/3-octave peaks) | `tonal_balance.rms_deviation_db` ≥ 2 | cap boosts below 40 Hz at +2 dB and above 8 kHz at +2.5 dB; drop bands within ±0.5 dB |
| corrective_eq | `highpass` 20 Hz | always for streaming/club | - |
| dynamics | `multiband` | crest factor above the genre target, or a band clearly out of control | remove when the mix already sits within 2 dB of the crest target; raise thresholds 2 dB for a lighter touch |
| dynamics | `compress` (glue) | crest factor above target | remove on dense, already-compressed mixes |
| stereo | `widen` | correlation ≥ 0.5 and the producer wants width | remove when correlation < 0.4; keep `mono_below` |
| color | `saturate` (tape, exciter) | thin, sterile mixes | remove on dense, distorted, or lo-fi mixes; lower drive on bright mixes |
| loudness | `gain` | always | re-level after other edits (below) |
| loudness | `limit` | always | `release` 50-150 ms; longer is smoother, shorter is louder |

## Re-levelling after an edit

The `gain` stage was computed for the plan as proposed. Removing or softening anything
before it changes the level that reaches the limiter: on a test pop mix, deleting the glue
compressor made the plan land at -13.4 LUFS and fail `master verify`. Run edited plans with
`--relevel`:

```bash
ferraus master mix.wav --plan plan.json --relevel -o master.wav --report r.json --json
```

It runs every stage before the loudness stages as written, then repeats the planner's
gain/limiter search (same target, intensity bound and limiter release) and records the new
values in the report's `plan` and `notes`. Without `--relevel`, `master --plan` runs the file
exactly, which is what you want only when you set the gain yourself.

If the report says the target was not reached (`loudness.reached: false`), the limiter hit
the intensity bound: the target is too loud for this mix with this chain. Accept the gentler
master at a lower custom target (`--target custom --lufs -15LUFS`), raise `intensity` in
the plan, or restore some dynamics stages.

## Matching a reference

`--reference ref.wav --match-amount 50%` moves the corrective EQ toward the reference's
tonal balance (capped at 4 dB per band). Raise the amount only when the reference is the
same genre, arrangement density and era. `ferraus analyze similarity master.wav ref.wav`
afterwards should rise; if it does not, the difference is in the mix, not the EQ.
