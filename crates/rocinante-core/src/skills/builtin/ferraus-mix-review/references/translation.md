# Translation checks

## Streaming normalization

Services play tracks at a reference loudness. For a master at integrated loudness L:

| Service | Reference | Playback change |
|---|---|---|
| Spotify (normal), YouTube, Tidal, Amazon, Deezer | about -14 LUFS | louder masters are turned down by L - (-14) dB; quieter ones are turned up only on some services and only as far as the peaks allow |
| Apple Music (Sound Check) | about -16 LUFS | turned down by L - (-16) dB |
| SoundCloud, Bandcamp, downloads | none | played as is |

Report the numbers: "At -8.5 LUFS this master plays 5.5 dB quieter on Spotify and 7.5 dB
quieter on Apple: the density it bought costs punch and gains no loudness there." A loud
master is a valid choice for clubs and DJs; say which destination it suits.

Preview what normalization does with a loudness-matched copy:
`ferraus normalize song.wav --lufs -14 -o song.at14.wav --json` (with the true-peak limiter
on by default, a quiet master turned up may be limited, as on Spotify "loud").

## Mono

`ferraus analyze stereo song.wav --json`: `mono_loss_db` above 3 dB, a negative
`correlation`, or `correlation_below_120hz` under 0.85 mean parts disappear on phones in
landscape, Bluetooth speakers, smart speakers, clubs and mono radio. To find the culprit
with stems, measure each bus; without stems, the per-band `bands[]` show where the width is.
`ferraus channels song.wav --to mono -o song.mono.wav` makes a mono version to compare
(`analyze similarity song.wav song.mono.wav`).

## Small speakers

Phone and laptop speakers reproduce little below 100-150 Hz. A bass or 808 whose energy is
almost all below 80 Hz vanishes there; harmonics (saturation) make it audible. Measure the
bass stem or the 100 Hz-1 kHz bands of the mix against the 40-80 Hz bands: a low end that is
more than 15 dB stronger below 80 Hz than at 100-300 Hz will disappear on a phone. Fix:
saturation on the bass (`saturate:type=tube,drive=6dB,mix=25%`) in the mix.

## Loud playback

At high volume, 2-5 kHz excess becomes painful first: the `harsh` check in `mix verify`.
