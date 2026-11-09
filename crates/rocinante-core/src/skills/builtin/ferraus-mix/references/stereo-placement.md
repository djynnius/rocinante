# Stereo placement

Panning decides how much room every part gets. A good stereo picture is balanced, keeps
the low end and the lead in the middle, gives parts with similar ranges their own sides,
and collapses to mono without losing anything important. All of that is measurable.

## Tools

| Situation | How | Notes |
|---|---|---|
| mono stem, off centre | `{ op: pan, position: -40 }` at the end of its chain, before the fader | `L30`/`R60` also accepted; constant-power -3 dB law |
| mono stem, centred | nothing: a mono input is copied to both sides | do not give the bus `mix` stage `pans`: with `pans`, `mix` applies the -3 dB law to every mono input and the parts play 3 dB under what `mix balance` measured |
| stereo stem: move its centre | `{ op: widen, center_pan: L20 }` | keeps the image intact |
| stereo stem: narrower or wider | `{ op: widen, amount: 70%, mono_below: 150Hz }` | 0% mono, 100% unchanged, 200% very wide |
| stereo stem: lean one side | `{ op: pan, mode: balance, position: -30 }` | turns the far channel down |
| fake stereo (correlation > 0.98), to pan | `{ op: channels, to: mono }`, then pan it | a stereo file of a mono source |
| fake stereo or mono, to make wide | `{ op: chorus, mix: 50% }` on the 2-channel file, then `widen` | measured on a fake-stereo pad: correlation 1.0 → 0.69 with no mono loss; `widen` alone, a short ping-pong delay or reverb barely moved it |
| movement | `{ op: autopan, rate: 1/2, depth: 40% }` or `{ op: pan, points: "0s=-60,8s=60" }` | fx, risers, arps |
| mono part or a mono stack of several voices, to make wide (Haas) | `{ op: channels, to: stereo }`, then `{ op: phase-align, mode: shift, shift: "L=0ms,R=20ms" }` (12-25 ms) | measured on a mono source: correlation 1.0 → 0.09, mono fold-down 2.6 dB quieter. Backing vocals, doubles and pads only, never the lead, bass or kick (in mono it comb-filters); write the chain in YAML (the `--fx` string form cannot carry the comma in `shift`) |

## The default map (pop; other genres below)

Positions in ferraus pan units. "Pair" means two parts of similar range mirrored.

| Part | Position | Width (stereo stems) |
|---|---|---|
| kick, snare, bass, sub/808 | 0 | mono below 120 Hz |
| lead vocal | 0 | mono |
| lead vocal double | pair ±100 at -8..-12 dB, or none | |
| backing vocals | stereo stem: 0 at 130-160%; mono parts: pair ±40..±80 | wider than everything except pads |
| hats | ±20..30 (choose a side) | |
| shaker, tambourine, percussion | opposite the hats, ±30..60 | |
| toms | spread from -60 (floor) to +60 (high), audience view | |
| overheads, room | as recorded, 100% | |
| rhythm guitar, two takes | ±80..100 | |
| rhythm guitar, one take | ±40..70, opposite the keys or a second guitar | |
| keys, piano (stereo) | 0, 80-100% width; or centre-pan ±20 opposite the guitar | |
| keys, piano (mono) | ±30..50 opposite the guitar | |
| pads, strings | 0 at 130-150%, `mono_below: 200Hz` | widest bed |
| synth lead | 0..±15 (centre only when no vocal plays at the same time) | |
| call and response: parts that trade phrases with each other or answer the vocal (harmonica, slide, a second guitar) | small mirrored offsets, ±5..25; the part that plays alone at that moment stays centre | the "handoff": each gets a sliver of its own space without leaving the middle |
| arps, plucks | ±30..60, or autopan with depth 30-50% | |
| brass section | ±20..40 | |
| fx, risers | wide, moving | |
| reverb returns | 100% | `widen` at most 120% |
| delay returns | ping-pong or 100% | |

## Rules behind the map

1. **Low end in the middle.** Everything below about 120 Hz is mono (bass, kick, sub, and
   `mono_below` on every stereo stem). Vinyl cutting, club systems, phone speakers and
   Bluetooth mono all punish wide lows, and the low end carries the most energy.
2. **The lead owns the centre between 1 and 5 kHz.** Anything else that lives there and plays
   at the same time moves off centre.
3. **Mirror similar ranges.** A guitar at L50 is answered by keys at R50; hats on one side
   by a shaker on the other. Energy, not instrument count, must balance.
4. **Contrast is width.** Verses narrower, choruses wider: if the arrangement adds BVs, pads
   or doubled guitars in the chorus, put them wide so the chorus opens up.
5. **Depth before width.** A crowded mix is more often fixed by pushing parts back (lower
   level, darker, wetter: space-and-movement.md) than by panning them further out.
6. **Do not chase width.** Width that collapses in mono is worse than no width.
7. **Keep printed stereo that works.** Stems that arrive as stereo with the producer's
   placement baked in (a stereo BV stack, a keys part already spread) keep it; move them only
   when a gate or a masking region says so, and say so in the report.
8. **Handoffs get slivers.** Parts that answer each other (call and response) are clearer a
   few units apart (±5..25) than stacked in the centre, and still read as one band. Panning
   them hard is a production choice, not a fix.
9. **One pulse in the middle.** When a single percussion part carries the subdivision (the
   only 1/8 or 1/16 pulse, as when the drums arrive as one stereo sum without separate hats),
   keep it near centre (0..±15): the groove needs a focus point. Two shakers panned hard
   apart smear it; keep one, or tuck the second low.
10. **Width needs side signal.** `widen` scales the difference between the channels; a mono
   or fake-stereo source has none. Create it (chorus, a stereo reverb or delay return,
   a double), or place the part with panning instead.

## Genre adjustments

| Genre | Changes from the default |
|---|---|
| hiphop | narrower overall: beat elements mostly ±0-30, 808 and kick mono, ad-libs ±40-70, doubles ±60-100 tucked low |
| edm | very wide pads and supersaws (150-180%) but `mono_below: 150Hz`; percussion spread; drop wider than build |
| rock | LCR: guitars hard ±100, everything else centre or overhead image; bass and kick dead centre |
| acoustic | natural stage picture: guitar ±20-40, piano 0 at 80%, vocal 0, shaker opposite guitar |
| jazz | audience perspective as a stage: piano 0..L30 wide, bass 0..R15, drums overhead image, horns ±20-50 |

## Width toolkit

When the brief asks for width and the mix measures narrow, add side signal; panning further
out only moves mono parts around. The chorus and `widen` rows were measured on a test
session; the others are standard techniques whose effect you measure on your own session:

| Move | Effect |
|---|---|
| `chorus` at `mix: 40-50%` on a fake-stereo or mono part kept 2-channel | correlation of the part 1.0 → about 0.69, no mono loss |
| wider BV and pad settings (`widen` 150-170% after the chorus) | widens the chorus without touching mono bass (`mono_below`) |
| a double of a mono part: `{ op: pitch, cents: 8 }` + `{ op: delay, time: 18ms, feedback: 0%, mix: 100% }`, panned opposite the original and 6-8 dB under it | strong width; check `mono_loss_db` (short delays comb-filter in mono) |
| more level on stereo returns (pads and BVs to the hall) | width that lives behind the dry parts |
| `delay` with `pingpong: true` on arps and plucks | movement and width in the gaps |
| Haas on a mono BV stack (`phase-align` shift, 12-25 ms) | strong width from one mono file; mono fold-down about 3 dB quieter on that part, so keep the part under the lead and re-check `mono_loss_db` on the mix |
| `widen` on a fake-stereo part | nothing: there is no side signal to scale |

Re-measure `correlation`, `mono_loss_db` and `correlation_below_120hz` after each move.

## Measure and gate

Run on the summed mix (and on each bus to find the culprit):

```bash
ferraus analyze stereo mix/mix.wav --json
```

| Metric | Pass | If not |
|---|---|---|
| `balance_db` | within ±1 dB (positive = left louder) | move the loudest off-centre part 10-20 units toward centre, or mirror it |
| `correlation` | ≥ 0.2 | too wide: lower `widen` amounts, check for a phase-flipped stem. High values are not a fault: a mix built mostly from mono sources measures about 0.9 however well it is panned |
| `correlation_below_120hz` | ≥ 0.9 | add `mono_below: 120Hz` to the widest stereo stems; centre bass/kick |
| `mono_loss_db` | ≤ 3 dB | narrow the widest parts; check reverb `width` |
| per-band `width` (`bands[]`) | lowest band near 0 | as above |

Then check that the lead survives mono: render the mix with `{ op: channels, to: mono }` and
confirm the vocal-to-music balance moves by less than 1 LU.
