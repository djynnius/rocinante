# Gates

A gate is a measurement with a pass window. The mix moves to the next phase only when its
gate passes, or when the report explains with numbers why it was left failing. Windows are
hand-designed starting points from common mixing practice (the same caveat as the genre
curves in `ferraus master`); a reference track, when the producer gives one, overrides them:
measure the reference the same way and use its values ±1 dB as the window.

## Order

| # | Gate | Measure | Pass |
|---|---|---|---|
| 1 | Intake | `info`, `analyze loudness` per stem | one sample rate; lengths within 10 ms; no stem clipping (true peak ≤ 0 dBTP) or flagged |
| 2 | Balance | the `balance` stage (`mix balance` on processed stems) | every correction within ±0.5 dB |
| 3 | Dynamics | stage `result.metrics` / `chain_metrics` | gain reduction inside the role ranges (role-recipes.md) |
| 4 | Masking | `mask_*` stages (`analyze masking`, level-aware) on post-fader stems | no `regions` on any priority pair with `co_activity` ≥ 0.2 |
| 5 | Stereo | `analyze stereo` on the mix | stereo-placement.md table |
| 6 | Vocal-to-music | `analyze loudness` on a cappella and instrumental; the `vox_ride` stage | genre window below; ride `offset_after.spread_lu` ≤ 3 LU (the vocal holds its place word by word) |
| 7 | Space | `analyze loudness` on each return family and its dry bus | genre windows below |
| 8 | Headroom | `analyze loudness` on the mix | true peak ≤ -3 dBTP; aim for -24..-14 LUFS (`mix verify` fails outside -26..-12) |
| 9 | Final | `mix verify` | `pass: true` |

## Vocal-to-music balance

`V2M = (integrated LUFS of the a cappella + its final gain) - (integrated LUFS of the instrumental + its final gain)`

Both measurements are gated (BS.1770), so silence between vocal phrases does not drag the
vocal number down.

The same balance seen from the full mix: `vocal-to-mix = LUFS(a cappella) - LUFS(mix)`.
V2M -4..0 corresponds to a vocal 3-6 LU under the full mix, which is where an informal
analysis of the top 25 Spotify tracks of 2023 put lead vocals (about 4.5 LU under the full
track, typically 3-6; Mastering The Mix, "The Perfect Vocal Loudness (Backed by Data)").
Treat the windows as conventions with that much evidence behind them, not as research
results.

| Genre | V2M window (LU) | Character |
|---|---|---|
| pop | -4 .. 0 | vocal clearly on top |
| hiphop | -4 .. 0 | vocal forward, drums and 808 right behind |
| rock | -6 .. -2 | vocal inside the band |
| edm | -7 .. -2 | vocal as one element of a loud track |
| acoustic | -3 .. +1 | voice and instrument as equals |
| jazz | -6 .. -1 | ensemble balance |

Outside the window: change the a cappella's gain in the `final` stage by the distance to the
window's centre, then re-check gates 5 and 8. Expect the genre template from `mix balance`
to leave the vocal 1-3 dB above the centre; that is why V2M is set in `final`, after the
template. If the vocal sits well inside the window but
the report from masking still lists vocal regions, fix masking first; level is the last
resort for intelligibility, not the first.

### Per-section check

`ferraus analyze loudness prints/acapella.wav --timeline --step 1s --json` and the same for
the instrumental give short-term curves. Compute V2M per section (verse, chorus, bridge:
section times from the producer, or from `analyze sections` on the rough mix).
A section more than 1.5 LU away from the song's V2M gets a gain ride on the vocal bus:

```yaml
- id: vocals_ride
  op: envelope
  in: vocals_bus
  args: { points: "0s=0dB,28.6s=0dB,28.8s=+1dB,48s=+1dB,48.2s=0dB" }
```

## Space (returns level)

Each return family against its own dry bus, both measured before the `final` stage so the
final gains cancel out (the template's `*_level` stages):

`vocal space = LUFS(vocal_returns) - LUFS(vocals_bus)`;
`instrument space = LUFS(inst_verb) - LUFS(inst_dry)`

| Genre | Vocal returns vs dry vocal (LU) | Instrument returns vs dry instrumental (LU) |
|---|---|---|
| pop | -18 .. -10 | -24 .. -14 |
| hiphop | -22 .. -12 | -28 .. -16 |
| edm | -18 .. -8 | -22 .. -12 |
| rock | -20 .. -12 | -24 .. -14 |
| acoustic | -16 .. -8 | -20 .. -12 |
| jazz | -18 .. -10 | -22 .. -14 |

The tested template sits at -14.2 (vocal) and -20.4 (instruments) for pop. Wetter than the
window: lower the sends with the biggest send levels first, or shorten decay. Drier: raise
BV and pad sends before the lead vocal's.

## Section contrast

Not a pass/fail gate, a sanity check: in `analyze loudness --timeline` of the mix, choruses
usually sit 1-3 LU above verses. If processing flattened that (a heavy bus compressor, or a
leveler someone added), back the bus compression off. Never add a leveler to music.

## When a gate will not pass

After 3 revisions, stop and write down:

- the gate, the window, the last value, and what you changed each time;
- the likely cause (a stem that is distorted at the source, an arrangement clash such as two
  lead lines in the same register at the same time, a reference that is not comparable);
- what the producer could do about it (re-export, re-record, mute in the clash sections).

A clear failure report is more useful to a producer than a mix bent out of shape to pass.
