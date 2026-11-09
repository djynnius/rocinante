# Review checklist

Thresholds for a **mix** unless marked *(master)*. "Fix" is a runnable `fx` stage on the
bounce, or the mix-pipeline stage to change when stems exist.

## Level and dynamics

| Check | Measure | Threshold | Why it matters | Fix |
|---|---|---|---|---|
| headroom | true peak (`analyze loudness`) | mix ≤ -3 dBTP; *(master)* ≤ the target ceiling (-1 dBTP streaming) | mastering needs room; inter-sample peaks distort on playback and in lossy encoding | mix: `gain:by=<-x dB>`; master: re-run `ferraus master` |
| clipping | `analyze problems` `clipping` | none | audible crackle; cannot be undone | re-bounce; `declip` as a repair |
| squashed mix | PLR (true peak - integrated) | mix ≥ 10 dB | a limited mix leaves mastering nothing to work with and sounds flat | re-bounce without the bus limiter or clipper |
| loudness *(master)* | integrated LUFS | target ± 0.5 LU | louder than the target is turned down by streaming and only costs punch | `ferraus master --target <t>` |
| section contrast | `loudness_range_lu`; timeline | pop/rock 4-8 LU, EDM 3-6, acoustic/jazz 6-12 | a flat LRA makes the chorus no bigger than the verse | less bus compression; arrangement |
| crest factor | `crest_factor_db` | mix 14-20 dB; *(master)* genre dependent (pop 9-12) | transients = punch | less limiting/compression |

## Balance

| Check | Measure | Threshold | Why | Fix |
|---|---|---|---|---|
| vocal balance | vocal-to-music (separated stems or the mix's prints) | genre window (pop -4..0 LU) | the most common complaint about a mix | vocal fader / bus |
| low end | 40-120 Hz bands vs genre curve | within ±4 dB | thin or boomy on every system | bass/kick levels, `eq` low shelf |
| mud | 200-400 Hz vs curve (`mix verify` `mud`) | ≤ +4 dB | masks vocals and clarity | `eq:band=peak:300Hz:-2dB:q=1.0` |
| harshness | 2-5 kHz vs curve (`mix verify` `harsh`) | ≤ +4 dB | ear fatigue at volume | `eq:band=peak:3.5kHz:-2dB:q=1.2` |
| air | 10-16 kHz vs curve | within -6..+3 dB | dull or brittle | `eq:band=highshelf:10kHz:+2dB` |
| sibilance | `analyze problems` `sibilance` | none above low | piercing s and t | `deess` on the vocal |
| sub rumble | below 40 Hz vs curve (`mix verify` `sub_rumble`) | ≤ +4 dB | wastes headroom, muddies the low end | `highpass:freq=30Hz,order=4` |

## Stereo

| Check | Measure | Threshold | Why | Fix |
|---|---|---|---|---|
| phase | broadband `correlation` | ≥ 0 (warn below 0.2) | parts cancel in mono | find the inverted stem; `invert-phase` |
| mono bass | `correlation_below_120hz` | ≥ 0.85 (mix), ≥ 0.9 (master) | wide bass collapses on clubs, vinyl, mono speakers | `widen:mono-below=120Hz` |
| mono loss | `mono_loss_db` | ≤ 3 dB | the mix shrinks on phones and smart speakers | narrow the widest parts |
| balance | `balance_db` | within ±1 dB | lopsided image | pan or level the loudest off-centre part |
| width | `correlation` | 0.3-0.8 typical for modern pop | very narrow mixes sound small; very wide ones hollow | widen/narrow pads and BVs |

## Edges and format

| Check | Measure | Threshold | Fix |
|---|---|---|---|
| head silence | `master verify` / `mix verify` `head_silence` | ≤ 1 s | `silence trim --edges` (masters only) |
| fades | `analyze loudness --timeline` end | no abrupt cut at the end | `fade:out=2s` |
| DC | `dc_offset` | ≤ 0.001 | `dc-remove` |
| format *(master)* | `info` | distributor spec (24-bit WAV 44.1/48 kHz; CD 16/44.1) | `master deliver` |
