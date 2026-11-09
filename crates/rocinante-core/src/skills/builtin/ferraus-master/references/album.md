# Albums and EPs

An album is mastered as a set: one ceiling, consistent tone, and a loudness map that
serves the sequence. Streaming services in album mode keep the relative levels of tracks,
so a ballad mastered as loud as the single sounds wrong in context.

## Procedure

1. Analyze every track:
   `ferraus master analyze track01.wav --genre <g> --target streaming --json` (one per track).
2. Choose the **anchor**: the loudest, densest track (usually the single). Master it first
   to the target and approve it.
3. Build the **loudness map**: each track's integrated target relative to the anchor.
   Conventional offsets: up-tempo tracks 0 to -1 LU, mid-tempo -1 to -2 LU, ballads and
   interludes -2 to -4 LU. Write the map into the notes.
4. Master each track with `--target custom --lufs <map value> --true-peak -1dBTP`, same
   genre, and the same intensity as the anchor unless its analysis says otherwise.
5. **Tone consistency**: with `--reference anchor_master.wav --match-amount 30%` on the
   other tracks, the corrective EQ leans them toward the anchor's balance. Check with
   `ferraus analyze similarity anchor_master.wav trackN_master.wav --json`; scores within the
   same album usually sit above 0.8.
6. Verify each with `master verify --target custom --lufs <map value>`.
7. Sequence and gaps: `ferraus concat` with `--gap` (2 s is a common default between songs)
   or `--crossfade` for segues, then measure the whole album once for a sanity check of the
   overall flow (`analyze loudness --timeline`).

## Report

A table: track, analysis summary (integrated, PLR, tonal deviation), target from the map,
landed loudness, limiter reduction, similarity to the anchor, verify result.
