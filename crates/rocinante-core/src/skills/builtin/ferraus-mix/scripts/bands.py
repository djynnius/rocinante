#!/usr/bin/env python3
"""Band summary of a stem or mix for mixing decisions, from `ferraus analyze spectrum`.

    python3 bands.py "stems/08 Lead Vox.wav"              # regions and resonance candidates
    python3 bands.py vox_before.wav --compare vox_after.wav   # did a move change what it should?
    python3 bands.py mix.wav --json

Regions (dB, relative to the file's loudest region): sub 20-60 Hz, low 60-120, low-mid
120-250, mud 250-500, mid 500-2k, presence 2-5k, brilliance 5-10k, air 10-20k.

Resonance candidates use one fixed rule, so every run means the same thing: a third-octave
band at least 6 dB above the median of the three bands on each side, counting only bands
within 30 dB of the file's loudest band (a vocal with nothing below 200 Hz does not make its
250 Hz band look like a resonance), and never one of the two lowest or two highest bands that
have energy (spectral edges are not resonances).

`--compare` prints region and band deltas (after minus before) so a move can be checked
against its purpose: an EQ cut at 300 Hz that leaves the mud region within 0.5 dB did
nothing and should come out again. Needs `ferraus` on PATH (or --ferraus PATH).
"""
from __future__ import annotations

import argparse
import json
import math
import statistics
import subprocess
import sys

REGIONS = [("sub", 20, 60), ("low", 60, 120), ("low-mid", 120, 250), ("mud", 250, 500), ("mid", 500, 2000),
           ("presence", 2000, 5000), ("brilliance", 5000, 10000), ("air", 10000, 20000)]


def spectrum(path: str, ferraus: str) -> list[dict]:
    p = subprocess.run([ferraus, "analyze", "spectrum", path, "--json"], capture_output=True, text=True)
    try:
        env = json.loads(p.stdout)
    except json.JSONDecodeError:
        sys.exit(f"ferraus analyze spectrum failed:\n{p.stderr}")
    if not env.get("ok"):
        sys.exit(f"ferraus analyze spectrum failed: {env.get('error')}")
    return [b for b in env["result"]["bands"] if b["db"] > -140]


def power_mean(dbs: list[float]) -> float:
    return 10 * math.log10(sum(10 ** (d / 10) for d in dbs) / len(dbs)) if dbs else float("-inf")


def regions(bands: list[dict]) -> dict[str, float]:
    raw = {name: power_mean([b["db"] for b in bands if lo <= b["center_hz"] < hi]) for name, lo, hi in REGIONS}
    top = max(v for v in raw.values() if v != float("-inf"))
    return {k: (round(v - top, 1) if v != float("-inf") else None) for k, v in raw.items()}


def resonances(bands: list[dict]) -> list[dict]:
    if not bands:
        return []
    loudest = max(b["db"] for b in bands)
    live = [b for b in bands if b["db"] >= loudest - 30]
    out = []
    for i, b in enumerate(live):
        if i < 2 or i >= len(live) - 2:
            continue
        around = [x["db"] for x in live[max(0, i - 3):i] + live[i + 1:i + 4]]
        excess = b["db"] - statistics.median(around)
        if excess >= 6:
            out.append({"center_hz": b["center_hz"], "excess_db": round(excess, 1),
                        "suggested_fix": f"eq:band=peak:{fmt_hz(b['center_hz'])}:{-min(6.0, round(excess / 2 * 2) / 2):g}dB:q=2"})
    return out


def fmt_hz(hz: float) -> str:
    return f"{hz / 1000:g}kHz" if hz >= 1000 else f"{hz:g}Hz"


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("file")
    ap.add_argument("--compare", help="a second file (after a move); prints after minus before")
    ap.add_argument("--json", action="store_true")
    ap.add_argument("--ferraus", default="ferraus")
    a = ap.parse_args()
    b1 = spectrum(a.file, a.ferraus)
    out = {"file": a.file, "regions_db": regions(b1), "resonances": resonances(b1)}
    if a.compare:
        b2 = spectrum(a.compare, a.ferraus)
        r1, r2 = regions(b1), regions(b2)
        # absolute region deltas (not relative to each file's loudest region)
        abs1 = {n: power_mean([b["db"] for b in b1 if lo <= b["center_hz"] < hi]) for n, lo, hi in REGIONS}
        abs2 = {n: power_mean([b["db"] for b in b2 if lo <= b["center_hz"] < hi]) for n, lo, hi in REGIONS}
        out["compare"] = a.compare
        out["region_delta_db"] = {n: (round(abs2[n] - abs1[n], 1) if math.isfinite(abs1[n]) and math.isfinite(abs2[n]) else None) for n in abs1}
        by2 = {b["center_hz"]: b["db"] for b in b2}
        out["band_delta_db"] = {fmt_hz(b["center_hz"]): round(by2[b["center_hz"]] - b["db"], 1) for b in b1 if b["center_hz"] in by2}
        del r1, r2
    if a.json:
        print(json.dumps(out, indent=1))
        return
    print(f"{a.file}: region levels relative to its loudest region (dB)")
    for k, v in out["regions_db"].items():
        print(f"  {k:<11} {'' if v is None else f'{v:+.1f}'}")
    if out["resonances"]:
        print("resonance candidates (band, excess over its neighbours, fix):")
        for r in out["resonances"]:
            print(f"  {fmt_hz(r['center_hz']):>8}  +{r['excess_db']} dB  {r['suggested_fix']}")
    else:
        print("no resonance candidates (no band 6 dB over its live neighbours)")
    if a.compare:
        print(f"after ({a.compare}) minus before, by region (dB):")
        for k, v in out["region_delta_db"].items():
            print(f"  {k:<11} {'' if v is None else f'{v:+.1f}'}")


if __name__ == "__main__":
    main()
