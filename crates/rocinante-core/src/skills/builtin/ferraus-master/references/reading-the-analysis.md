# Reading `master analyze`

`ferraus master analyze mix.wav --genre <g> --target <t> --json` returns, under `result`:

## `loudness`

| Field | Meaning | Decision |
|---|---|---|
| `integrated_lufs` | whole-song loudness (gated) | distance to the target is `dynamics.gain_to_target_db` |
| `short_term_max_lufs` | loudest 3 s | more than 4 LU above integrated: a loud section (drop, last chorus) will drive the limiter hardest |
| `loudness_range_lu` | macro dynamics (verse vs chorus) | keep it: losing more than 1-2 LU of it means the master flattened the arrangement |
| `true_peak_dbtp` | inter-sample peak | above -1 dBTP on a mix: little headroom; above 0: likely clipped |
| `crest_factor_db`, `plr_db` | micro dynamics (transients) | PLR below 8 dB on a "mix" = already limited |

## `dynamics`

| Field | Decision |
|---|---|
| `gain_to_target_db` | how far the master must raise the mix |
| `limiter_reduction_needed_db` | if the mix were only gained and limited, this much limiting is needed |
| `smallest_intensity_that_fits` | the lightest `--intensity` whose limiter bound covers that; `none` means even heavy (6 dB) is not enough: pick a quieter target or accept that the planner will compress first |
| `transient_density_per_s` | busy material (high density) tolerates less limiting before it pumps |
| `sections` | loudness outliers by time; in music these are usually intentional (intros, breakdowns): never "fix" them with a leveler |

## `tonal_balance`

Against the genre curve (or the `--reference`). `offset_db` is removed first, so only the
shape counts.

- `rms_deviation_db` below 2.5: tonally close to the genre; the corrective EQ should be
  gentle. 2.5-4: normal for an unmastered mix. Above 4: a mix issue in some region.
- `largest_deviations[]`: the bands to look at. Positive `delta_db` = too much.
- Deviations below 40 Hz and above 12 kHz are the least reliable (arrangement and genre
  vary most there); treat them as hints.
- Several adjacent bands off in the same direction (a "shelf") are a real tonal tilt; a
  single band off by 5 dB is a resonance or a single instrument: a mix note.

## `stereo`

| Field | Decision |
|---|---|
| `correlation` | below 0.2: very wide or phase problems (no widening; check the mix); above 0.9: near mono (gentle widening is allowed if the producer wants width) |
| `correlation_below_120hz` | below 0.9: wide bass; the plan's bass-mono stage must stay |
| `mono_loss_db` | above 3 dB: parts vanish in mono (phones, clubs, smart speakers): a mix note |

## `problems`

Each has `severity` and a `suggested_fix`. Clipping, clicks, DC and rumble are fine to fix in
mastering (the plan's preflight handles DC and the high-pass handles rumble). Treat
`loudness_outliers` with `leveler` as a false alarm on music.

## `context`

| Field | Decision |
|---|---|
| `looks_mastered`, `mastered_score`, `mastered_reasons` | already limited: see the SKILL's intake rule |
| `genre`, `genre_source`, `genre_reasons` | with `genre_source: auto`, confirm against the producer's genre; a wrong genre moves the EQ and dynamics targets |
| `tempo_bpm`, `tempo_confidence` | octave errors are common (100 detected as 200); multiband release times follow this value |
| `key`, `key_confidence` | relative major/minor confusion is common; informational only |
