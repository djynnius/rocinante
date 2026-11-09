---
name: ferraus-stems
requires: [ferraus]
description: Prepare, check, fix, split, and repurpose stems with the ferraus CLI - validate a stem export (lengths, rates, start alignment, does it sum back to the bounce), fix a bad note, slide, squeak or noise in a stem by patching it from a clean repeat or another take, separate a stereo song into vocals/drums/bass/other (and lead/backing vocals, kick/snare/hats), make alternate versions (instrumental, a cappella, TV mix, clean edit, performance track), and prep remix material (new tempo or key, loops, chops, MIDI from parts). Use this whenever someone has a folder of stems or a finished song and wants stems checked, fixed (a wrong note, fret noise, a squeak in one pass), or delivered to a label or sync library, wants stems from a song that has none, wants an instrumental/karaoke/a cappella/TV mix, or wants to remix, sample, flip, re-key, or re-tempo a track.
---

# Stems with ferraus

Stems are the currency of modern production: mixing, mastering, remixes, sync, live shows
and labels all ask for them. This skill gets them right (complete, aligned, labelled), makes
them when they do not exist, and turns them into the versions people ask for.

Run ferraus with `--json`; check flags with `ferraus docs <command>`.

## Validate a stem export

A good export: one sample rate and bit depth, every file the same length, all starting at
the song's bar 1 (silence included), no clipping, and the stems sum back to the full mix.

```bash
ferraus info "stems/01 Kick.wav" --json          # every stem: rate, channels, bit depth, duration
ferraus analyze loudness "stems/01 Kick.wav" --json
ferraus mix stems/*.wav -o check/sum.wav --json  # every stem at 0 dB, unpanned
ferraus compare song_bounce.wav check/sum.wav --json
```

| Check | Pass | Common cause when it fails |
|---|---|---|
| sample rates | all equal | a stem bounced from a different session |
| durations | all within 10 ms | a stem exported from its first note instead of bar 1 |
| `compare` `offset_s` | 0 (within 1 ms) | the export started at a different position |
| `compare` `null_depth_db` | -40 dB or deeper | master-bus processing (compressor, limiter) on the bounce but not on the stems; a muted or missing stem; stems exported post-fader with automation the bounce lacked |
| clipping | `analyze problems` reports none | a stem printed hot through a bus |

A shallow null with the bounce louder than the sum usually means the bounce went through
the master bus chain: report it ("stems are pre-master; the bounce is mastered"), it is not a
defect. A null that is shallow only in some sections points to a missing stem: measure the
difference file (`ferraus mix song_bounce.wav check/sum.wav --gains 0dB,0dB` with the sum
phase-inverted first: `ferraus invert-phase check/sum.wav -o check/sum_inv.wav`) with
`analyze loudness --timeline` to see where.

Deliver with consistent names (`<Artist> - <Title> - <Stem>.wav`, numbered in mix
order), one format (`ferraus convert ... --bit-depth 24` and `ferraus resample --to 48k`
when a spec asks), and a `STEMS.md` listing tempo, key, sample rate, length, and whether the
stems are pre- or post-master.

## Fix a part before the mix

Engineers fix what they cannot mix around first: a sour note or a fret slide in one pass, a
squeak, a chair creak, a cough. Do it on the stem before any processing, keep the original,
and list every edit in the report so the producer can undo it.

| Problem | Find it | Fix |
|---|---|---|
| a bad note, slide or squeak in one pass of a part that repeats | the producer's note; for a squeak or noise above the part's range, the high-band timeline below; for notes, `transcribe` the part and compare with `analyze chords` on the bounce (an off-chord note is a candidate, not a verdict: passing and blue notes are deliberate) | `patch` the same bars from a clean pass |
| one click, creak or cough | `analyze problems`, `analyze spectrum` on a short `trim` around it | `repair --range <event> --freq <its band>` |
| a noise that recurs (a squeaky chair, a buzz) | a stretch where only that noise plays | `remove-sound --learn <that stretch>` |
| a noise in a gap between phrases | `analyze silence` regions with a high `floor_db` | an `envelope` dip over the gap |

`analyze problems` catches clicks, not a squeak or a noise burst that lasts a few hundred
milliseconds. When the noise sits above the part's own range (a squeak on a bass, a chair
creak under a pad), high-pass the stem above the part and read the loudness timeline:

```bash
ferraus highpass bass.wav --freq 3kHz --order 4 -o check/bass_hi.wav --json
ferraus analyze loudness check/bass_hi.wav --timeline --step 100ms --json
```

The band is near silence except where the noise is: on a test bass with a 0.4 s burst added,
eight 100 ms steps rose from below -70 to as high as -15 LUFS. Momentary loudness uses a
400 ms window, so the event starts up to 0.4 s before the first loud step.

Finding the clean repeat: run `analyze sections` on the bounce or the rough mix, not on the
one stem: its `repeats` list pairs of sections with `offset_s`, so a bad bar at time t in one
pass has its twin at t - offset_s in the earlier pass (or + offset_s in the later one).
Confirm the twin plays the same part before patching. Do not compare the bad bar with it (the
damage makes them differ); compare the bars on either side with theirs:

```bash
ferraus analyze sections song_bounce.wav --bpm 100 --json
ferraus trim bass.wav --range 57.6s..60s --snap off -o check/before.wav --json
ferraus trim bass.wav --range 28.8s..31.2s --snap off -o check/before_twin.wav --json
ferraus analyze similarity check/before.wav check/before_twin.wav --json
ferraus patch bass.wav --range 60s..62.4s --from 31.2s -o fixed/bass.wav --json
```

A played part never nulls against its repeat (timing and dynamics differ), so `compare` is
not the check; `analyze similarity` scoring `same` on the bars either side is. A programmed
loop that repeats exactly also nulls (`compare --no-align` deeper than -30 dB), and then the
patch is invisible. Measured on the test session (a humanized bass, a noise burst added in
the second chorus): `analyze sections` placed both choruses exactly (`offset_s` 28.81), the
bars either side scored 0.93 and 0.94 (`same`), the patch reported `level_difference_db`
0.22, the 4-8 kHz band of that second went from about -30 dB back to about -105 dB like the
clean take, and everything outside the edit was unchanged (null -142 dB).

- Patch whole beats or bars (`--range 33b..34b --from 17b --bpm 108`), so both edges land at
  the same place in the groove. The default 10 ms equal-power crossfades suit a part played
  again; `--crossfade-curve linear` suits a sample-identical loop.
- `level_difference_db` in the result says how much louder the replacement is than the
  stretch it replaced. A pass played softer or harder gets `--gain` (when the stretch you
  replaced was itself representative).
- Comping a phrase from another take recorded to the same click: `--source take3.wav`, with
  `--from` at the same time as `--range` starts.
- Check: the stem's duration is unchanged (`info`), and `analyze problems` reports no new
  clicks at the edges.

## Separate a song into stems

```bash
ferraus separate song.wav --stems 4 -y --json            # vocals, drums, bass, other
ferraus separate song.wav --stems 6 -y --json            # adds guitar, piano
ferraus separate song.wav --stems 2 --instrumental -y --json   # vocals + one instrumental file
ferraus separate song.wav --stems 4 --vocal-split --drum-split -y --json   # lead/backing vocals; kick, snare, toms, hh, cymbals
```

- Separation models download on first use (`-y` agrees; without it ferraus exits 5 with the
  `ferraus models pull` hint). `--tier fast` for a sketch; the default picks the best
  verified tier.
- `--sum-to-mix on` (default) assigns the residual to `other`, so the stems add back to
  the input exactly: safe for remixing.
- Separated stems leak (a bit of hi-hat in the vocal, reverb tails split between stems).
  Fine for remixes, practice and rebalancing; for a commercial instrumental, prefer the
  producer's real stems.
- Check a separation: `ferraus analyze similarity` of the stem sum against the song should
  say `same`; listen-free spot check of leakage: `analyze spectrum` on the vocal stem in a
  section where only drums play (it should be 20 dB or more under its singing level).

## Alternate versions

Build every version from the same stems and processing so they match the main mix sample
for sample (the `ferraus-mix` pipeline template already produces the instrumental and a
cappella). With only stems and no mix pipeline:

| Version | Recipe |
|---|---|
| instrumental | `ferraus mix` of every stem except lead and backing vocals |
| a cappella | lead + backing vocals (with their effects) |
| TV mix | instrumental + backing vocals (no lead) |
| performance / show track | instrumental + backing vocals + any parts not played live |
| clean edit | the main mix with `envelope` dips on the vocal stem at each explicit word (`points: "61.2s=0dB,61.22s=-60dB,61.6s=-60dB,61.62s=0dB"`), or a reversed word (`trim` + `reverse` + `mix --offsets`) |
| radio edit (shorter) | `split --at` the section boundaries, `concat --crossfade 20ms` the chosen sections |
| DJ intro/outro | `concat` 8-16 bars of drums-only from the drum stem before the song |

Master alternate versions with the main master's plan so their processing is identical
(`ferraus master version.wav --plan plan.json`; see the `ferraus-master` skill).

## Remix prep

| Task | Command |
|---|---|
| tempo and key of the source | `ferraus analyze tempo drums.wav --json`, `ferraus analyze key other.wav --json` (check octave and relative-key errors) |
| new tempo | `ferraus stretch vocals.wav --bpm 92 --to-bpm 124 -o vocals_124.wav` (`--mode percussive` for drums, `tonal` for pads) |
| new key | `ferraus pitch vocals.wav --semitones -2 -o vocals_down2.wav` (formants preserved by default) |
| bars and loops | `ferraus trim drums.wav --range 32b..40b --bpm 124 -o drum_loop.wav`; `ferraus split drums.wav --every 4b --bpm 124` |
| chops | `ferraus split vocals.wav --on-silence --min-silence 250ms` (one file per phrase) |
| parts as MIDI | `ferraus transcribe bass.wav -o bass.mid --quantize 1/16` (mono parts), `--mode poly` for chords |
| groove of the original | `ferraus midi groove extract drums.mid -o groove.json`, then `midi quantize --groove groove.json` on new parts |

Name the outputs with tempo and key (`vocals_124bpm_Am.wav`) so the remixer never has to
measure again; ferraus also records both in each output's `.ferraus.json` sidecar.
