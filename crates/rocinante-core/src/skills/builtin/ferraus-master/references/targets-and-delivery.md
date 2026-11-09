# Targets and delivery

## Named targets (`--target`)

| Target | Integrated | True peak | Notes |
|---|---|---|---|
| streaming | -14 LUFS | -1 dBTP | Spotify, YouTube, Tidal, Amazon normalize around this level |
| apple | -16 LUFS | -1 dBTP | Apple Music and Apple Podcasts guidance |
| youtube | -14 LUFS | -1 dBTP | alias of streaming |
| club | -8 LUFS | -0.3 dBTP | DJ and club playback, no normalization |
| cd | -10 LUFS | -0.3 dBTP | 16-bit 44.1 kHz with dither |
| broadcast | -23 LUFS | -1 dBTP | EBU R128; `--standard atsc` gives -24 LKFS / -2 dBTP |
| podcast | -16 LUFS | -1 dBTP | -19 LUFS for mono; speech chain |
| custom | `--lufs` | `--true-peak` | anything else |

## Which masters to make

- **One streaming master** covers Spotify, YouTube, Tidal, Amazon, Deezer, SoundCloud. A
  master at -14 LUFS is played as made; louder masters are turned down to about -14 and
  lose the punch their limiting cost them.
- **Apple Music** normalizes to about -16 LUFS. The streaming master plays fine there (it is
  simply turned down 2 dB); make a separate -16 master only when the producer asks.
- **Club / DJ** wants a denser, louder master (-8 to -6 LUFS, -0.3 dBTP): a separate master,
  usually `--intensity heavy`. Check that the limiter stays at or below 6 dB of reduction;
  past that, transients smear.
- **CD** is 16-bit 44.1 kHz with dither, typically -10..-9 LUFS: a separate master.
- **Broadcast / sync** follows the broadcaster's spec (EBU R128 or ATSC A/85): separate.
- **Lossy files** (MP3, AAC): encoders add inter-sample peaks; -1 dBTP leaves room for that at
  320 kbps. For lower bitrates, master to -1.5 or -2 dBTP.

## Delivery files

```bash
ferraus master deliver master.wav --formats wav24,wav16,mp3,flac --tags title="Song",artist="Artist",album="Album",date=2026,track=3 --isrc US-XXX-26-00001 --loudness-sheet -o release/song.wav --json
```

- `wav24`: the archive and distributor master (original rate).
- `wav16`: 16-bit 44.1 kHz, TPDF dither (CD, some distributors).
- `mp3` 320 kbps, `aac` 256 kbps (needs FFmpeg; `ferraus doctor` says), `flac` 24-bit.
- `--loudness-sheet` writes `.loudness.json` and `.csv` with the measured numbers of every
  file: distributors and broadcasters ask for this.

After delivery, run `master verify` on the 16-bit WAV and MP3 too: dither and encoding can
move the true peak.
