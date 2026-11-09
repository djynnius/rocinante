---
name: ferraus-mix-review
requires: [ferraus]
description: Review a mix or master like a seasoned engineer giving notes - measured, prioritized feedback with a runnable fix for each issue, comparison against a reference track, A/B between versions, and a translation check (mono, phone speakers, streaming normalization) - without changing the audio unless asked. Use this whenever someone shares a bounce, demo, mix, or master and asks for feedback, a critique, "what's wrong with my mix", "is this ready to release", "does it sound pro", "why does it sound worse than the reference", "which version is better", or "how will it sound on Spotify/Apple/YouTube"; also before mastering, to decide whether a mix should go back for fixes.
---

# Mix and master review with ferraus

Producers want honest, specific notes they can act on: what is wrong, how much, why it
matters, and the exact fix. You cannot listen, so every note is a measurement with a
threshold, and every opinion that cannot be measured is labelled as such. Do not modify
the audio unless the producer asks; a review is a report.

Run ferraus with `--json`; check flags with `ferraus docs <command>`.

## 1. Measure

```bash
ferraus info song.wav --json
ferraus master analyze song.wav --genre <genre> --json          # add --reference ref.wav when there is one
ferraus mix verify song.wav --genre <genre> --json              # for a mix (exit 9 = failed checks, still read it)
ferraus master verify song.wav --target streaming --json        # for a master
ferraus analyze loudness song.wav --timeline --step 1s --json   # sections
ferraus analyze stereo song.wav --json
ferraus analyze problems song.wav --json
```

First decide what you are reviewing: `master analyze`'s `context.looks_mastered` (and a
PLR below about 10 dB) says it is a master; otherwise treat it as a mix. Review a mix
against mix standards (headroom, balance, tone) and a master against delivery standards
(loudness target, true peak, translation). Never criticize a mix for not being loud.

If a separation tier is already cached (`ferraus models list --cached`), or the producer
agrees to a download, split the bounce to check the vocal balance:
`ferraus separate song.wav --stems 2 --json` gives vocals and instrumental; then
vocal-to-music = LUFS(vocals) - LUFS(instrumental), judged with the windows in the
`ferraus-mix` skill's gates (pop -4..0 LU). Note that separation leaks, so treat the value
as ±1 LU.

## 2. Turn measurements into notes

Use [references/checklist.md](references/checklist.md): every check, its threshold, why it
matters to a listener, and the fix. Then prioritize:

1. **Blockers**: anything that will be audibly broken or rejected: clipping, out-of-phase
   bass, true peak over 0 dBTP on a master, a loudness target missed by a distributor spec,
   head silence over a second, a vocal that disappears in mono.
2. **Big wins**: the 2-3 changes that would move the track most toward the reference or
   genre: usually vocal balance, low-end balance (kick/bass), tonal tilt, and harshness.
3. **Polish**: everything else, briefly.

Each note: **what** (the measurement and where: time range or frequency band), **how
much** (number vs threshold), **why it matters** (one sentence in listener terms), and the
**fix** (a runnable `ferraus fx` stage or command; for mixes with stems, the stage to change
in the mix pipeline). Five strong notes beat twenty weak ones.

## 3. Compare

- **Against a reference**: `ferraus master analyze song.wav --reference ref.wav --json`
  gives per-band deltas after removing the overall level; `analyze similarity song.wav
  ref.wav` gives a 0-1 score. Compare loudness, PLR (density), LRA (section contrast),
  correlation (width), and the 3-4 largest tonal deltas. A reference in a different genre,
  era or arrangement density is a weak reference: say so.
- **Between versions** (mix v3 vs v4): loudness-match first or the louder one "wins": compare
  `analyze similarity`, and the deltas of every checklist item; `ferraus compare v3.wav
  v4.wav --json` (null test) shows how much actually changed (`null_depth_db`,
  `rms_diff_db`, `si_sdr_db`); a null deeper than about -40 dB means the versions are
  practically identical and the producer may be chasing something inaudible. Report which
  version passes more checks and where each is better.

## 4. Translation

[references/translation.md](references/translation.md): what streaming normalization will do
to this file (playback gain per platform), whether it survives mono (phones, clubs, smart
speakers), and how it will read on small speakers (low end that exists only below 80 Hz
disappears).

## 5. Report

```markdown
# Review: <title> (<mix | master>, <genre>)

**Verdict:** <ready | ready after N fixes | needs another mix pass>, one sentence why.

## Blockers
1. <what> - <number vs threshold> - <why it matters> - fix: `<command or stage>`

## Biggest wins
1. ...

## Polish
- ...

## Against the reference (<file>)
| | This mix | Reference |
|---|---|---|
| integrated / PLR / LRA | ... | ... |
| width (correlation) | ... | ... |
| largest tonal deltas | +4.1 dB @ 250 Hz, -3.2 dB @ 8 kHz | |

## Translation
Streaming: Spotify/YouTube play it <x> dB quieter, Apple <y> dB. Mono: <loss>. Small speakers: <note>.

## Not measurable (judgment calls)
- <e.g. the snare tone, the reverb character>: convention says <...>; your call.
```

If the producer then wants the fixes applied, hand off: mix fixes to the `ferraus-mix`
skill (with stems) or an `fx` chain on the bounce (without), mastering to `ferraus-master`.
