---
name: pixelox
requires: [pixelox]
description: Edit photos and graphics with the pixelox CLI — layers, masks, selections, blend modes, adjustments, filters, retouching, type, shapes, layer styles, templates, SVG, tracing, and scripted rendering. Use when the user wants to edit, grade, crop, resize, cut out, retouch, composite, watermark, or convert images; build posters, social cards, or stickers; author or rasterize SVG; or batch-process a folder of images.
---

# pixelox

pixelox is installed as the `pixelox` command. It never prompts and is built for agents.

## Iron rules

- Add `--json` to every call and read the envelope: `ok`, `outputs[].path`, `warnings`,
  `error.code` / `error.hint`.
- Run `--dry-run --json` before anything that writes to the user's files.
- Never pass `--overwrite` unless the user asked to replace a file. Never edit source
  images in place: work in a `.pxo` document (`pixelox open in.jpg -o doc.pxo`) or write
  a new file with `-o`.
- Download models (`-y`) only with the user's consent.
- Check your work before declaring success: `pixelox analyze diff a.png b.png --json`,
  `pixelox verify out.png.pixelox.json --json`.
- More than three steps: write a pipeline file and `pixelox pipeline run job.yml`.

## Find your way

- `pixelox describe --agent`: every command in one screen.
- `pixelox docs <command>` and `pixelox schema <command>`: exact flags and types.
- `pixelox info file --json`, `pixelox layer list doc.pxo --json`: inspect first.

## Addressing

`--layer name|#N|@top|@bottom|@active`; `--mask name|@sel|@alpha:LAYER|@mask:LAYER|mask.png` with
`--invert-mask`; geometry `X,Y,WxH` / `X,Y` / `WxH` in px, %, pt, mm, in (negative counts
from the right/bottom, `--anchor tl..br`); colors `#rrggbb[aa]`, `rgb()`, `hsl()`, names.

## Recipes

| Goal | Commands |
| --- | --- |
| Inspect | `pixelox info shot.jpg --json` |
| Resize / convert | `pixelox transform resize shot.jpg -o small.jpg --size 1600x` · `pixelox export doc.pxo -o out.webp` |
| Grade | `pixelox adjust curves doc.pxo --in-place --points '0,0 64,56 192,206 255,255'` · `pixelox adjust white-balance doc.pxo --in-place --auto` |
| Retouch a portrait | `pixelox select skin doc.pxo --in-place --at 1230,1590` → `pixelox retouch blemishes doc.pxo --in-place --mask @sel --min-redness 10 --heal` → `pixelox retouch frequency-split doc.pxo --in-place`; the full playbook is the pixelox-retouch skill |
| Cut out (any backdrop, local model) | `pixelox detect subject doc.pxo --in-place --save-mask subject -y` (first use downloads 224 MB: ask the user before -y; never trace by hand instead) → `pixelox mask refine doc.pxo --in-place --mask subject --decontaminate` → `pixelox export doc.pxo -o cut.png --apply-mask subject` |
| Effects and art | `pixelox render lens-flare doc.pxo --in-place --at 70%,20%` · `pixelox fx fire doc.pxo --in-place --layer Title` · `pixelox filter pencil photo.jpg -o sketch.png --color` (also charcoal, ink, oil-paint, watercolor, cartoon, cutout, halftone) |
| Cut out (plain backdrop) | `pixelox select color doc.pxo --in-place --ref white --at 4,4 --contiguous --tolerance 40` → `pixelox select invert doc.pxo --in-place` → `pixelox export doc.pxo -o cut.png --apply-mask @sel` |
| Blur the background | `pixelox filter gaussian doc.pxo --in-place --sigma 8 --mask subject --invert-mask` |
| Replace a sky | `pixelox select sky doc.pxo --in-place --layer Hills` → `pixelox select invert doc.pxo --in-place` → `pixelox mask add doc.pxo --in-place --layer Hills --from @sel` |
| Brush a mask or shadow | `pixelox paint stroke doc.pxo --in-place --layer Sky --target mask --points '0,70% 100%,72%' --size 400 --flow 10 --color black` |
| Final grade | `pixelox layer stamp doc.pxo --in-place --name Grade` → `pixelox adjust clarity doc.pxo --in-place --layer Grade --amount 25` · `adjust dehaze` · `adjust texture` |
| Type | `pixelox text add doc.pxo --in-place --content "Hello" --font sans-serif --size 48pt --at 50%,10% --anchor t` |
| Shapes and styles | `pixelox shape rect doc.pxo --in-place --rect 40,40,300x120 --radius 16 --fill '#0ea5e9'` · `pixelox fx drop-shadow doc.pxo --in-place --layer Title --offset 0,6 --blur 12` |
| Template | `pixelox comp template apply t.pxo --set headline="X" --set-image product=p.png -o out.pxo` |
| Watermark many | `pixelox batch run --glob 'in/*.jpg' --pipeline watermark.yml --out-dir out/ --jobs 8` |
| SVG | `pixelox svg rasterize logo.svg -o logo.png --scale 2` · `pixelox trace scan.png -o logo.svg --mode binary` |

## Errors

| Exit | Meaning | Do |
| --- | --- | --- |
| 2 | bad flag or value | read `error.message`; `pixelox docs <command>` |
| 3 | input missing | check the path |
| 4 | output exists | pick a new `-o`, or `--overwrite` only if asked |
| 6 | download consent needed | ask the user; then run `error.hint` |
| 9 | verification mismatch | an input changed; see `error.context` |
| 12 | not in this build | use the alternative in `error.hint` |

Each output has `<output>.pixelox.json`; `pixelox replay` rebuilds it and
`pixelox pipeline new -o p.yml --from-sidecar <sidecar>` turns it into a pipeline.
