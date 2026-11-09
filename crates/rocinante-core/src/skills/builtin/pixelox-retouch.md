---
name: pixelox-retouch
requires: [pixelox]
description: Retouch portraits with the pixelox CLI like a high-end beauty retoucher - find and heal blemishes, even skin tone with frequency separation, "airbrush" skin while keeping its pores, dodge and burn, and prove the result with before/after crops. Use this whenever someone wants a portrait, headshot, selfie, or beauty shot airbrushed, retouched, cleaned up, skin-smoothed, made "flawless" or "magazine ready", or wants pimples, acne, spots, blotches, or shine removed; when they mention frequency separation, the healing or patch tool, or a natural versus editorial skin look; and when they ask why retouched skin looks plastic, blotchy, or a different color. Works unsupervised: every change is masked to skin and measured.
---

# Portrait retouching with pixelox

You are the retoucher. pixelox does the pixel work; your job is the judgment: what is a
blemish and what is part of the person, how much smoothing the picture can take, and whether
the result still looks like the same person on a good day.

Run pixelox with `--json` and read `result`, `warnings`, and `error.hint`. Check flags with
`pixelox docs <command>`. Coordinates are full-resolution canvas pixels.

## Principles

1. **Keep the person.** Never change skin tone, face or body shape, or permanent features
   (moles, freckles, scars, birthmarks, the small dark marks common on darker skin) unless
   the user asks. Pimples, temporary redness, stray hairs, dust, and shine are fair game.
   When unsure, keep it and say so.
2. **Keep the texture.** Pores are what make skin read as skin. Smooth tone on the
   low-frequency layer; never blur the high-frequency (texture) layer. Plastic skin is a
   failed retouch, even when someone asked for "flawless".
3. **Skin only.** Every change goes through a skin mask from `select skin`: never touch
   eyes, brows, lashes, lips, teeth, hair, glasses, or clothes.
4. **Non-destructive and replayable.** Work in a `.pxo` on a copy of the photo; export at the
   end; the sidecar rebuilds every step.
5. **Prove it.** Compare 100% crops before and after, and check that the skin's color held.
6. **Ask before you trace.** When the job needs the person cut out (a new background, a
   composite) and `detect subject` stops with exit 6 (PX1502: its 224 MB model is not
   downloaded yet), ask the user to approve the download, then rerun with `-y`
   (`pixelox detect subject work.pxo --in-place --save-mask subject -y`), then refine the
   hair with `pixelox mask refine work.pxo --in-place --mask subject --decontaminate`. Do
   not quietly trace the outline by hand instead: a traced path loses hair and soft edges.
   Trace only when the user declines the download or works offline (then refine the traced
   mask the same way), and say that you did.

## Workflow

### 1. Open and look

```bash
pixelox info portrait.jpg --json
pixelox open portrait.jpg -o work.pxo
pixelox layer dup work.pxo --in-place --layer Background --name Retouch
pixelox export work.pxo -o preview.jpg --size 1000x
```

Find the face in the preview, then inspect skin at 100% with region crops
(`pixelox export work.pxo -o crop.png --region 1150,1200,700x700`). Note the light: soft
and even, or one side in deep shadow.

### 2. Skin mask

Sample skin, not features: a lit cheek, the forehead, and (for dramatic light) a cheek
in shadow and the neck.

```bash
pixelox select skin work.pxo --in-place --at 1230,1590 --at 1500,990 --at 1770,1590
pixelox select save work.pxo --in-place --as skin
pixelox mask export work.pxo --mask skin -o skin-mask.png
```

Look at the mask and read `advice`. Fixes: the shadow side is missing → add an `--at` in
the shadow or raise `--lightness` to 80; the neck or hands are missing → add `--at` there or
`--contiguous off`; lips, hair, or a skin-colored backdrop leak in → lower `--tolerance`.

### 3. Blemishes (before splitting)

List first, then heal:

```bash
pixelox retouch blemishes work.pxo --layer Retouch --mask skin --json
pixelox retouch blemishes work.pxo --in-place --layer Retouch --mask skin --min-redness 10 --heal
```

Read the spot list. `redness` above about 10 means an inflamed pimple or blotch; around 0
or below means a mole, freckle, or permanent mark: keep those (`--min-redness 10` does). The
defaults are conservative; for faint spots raise `--sensitivity` in steps of 5 (55, 60, 65)
and on large photos lower `--min-size` (for example 5). Stop when the count jumps and the
contrasts sink toward the floor: that is pore texture, not blemishes. To review before
healing, add `--save-mask spots --in-place` and `pixelox mask export work.pxo --mask spots
-o spots.png`. Heal anything left by hand with `pixelox retouch heal work.pxo --in-place
--layer Retouch --at 812,640 --radius 9`.

### 4. Frequency separation

```bash
pixelox retouch frequency-split work.pxo --in-place --layer Retouch
```

This builds a group with `Low` (tone and color) and `High` (texture, linear light) that
reproduces the photo exactly (`reconstruction.max_error_levels` is 0 or close).
Heal blemishes before this step: healing the source afterwards has no visible effect.

### 5. Even the tone on Low (the airbrush)

```bash
pixelox filter surface-blur work.pxo --in-place --layer Low --radius 12 --threshold 18 --mask skin
```

Surface blur is the Mixer Brush done safely: it averages tone within each tonal zone and
stops at edges, so highlights and shadows do not smear into each other.

| Look | `--radius` (of the long side) | `--threshold` |
| --- | --- | --- |
| Natural (headshots, weddings) | about 0.3% | 12-18 |
| Editorial (beauty, covers) | 0.5-0.8% | 20-30 |

Go stronger in a second pass on the cheeks only (a smaller mask) rather than raising the
strength everywhere.

### 6. Texture touch-ups on High (optional)

Heal a distracting pore cluster or stray hair with a small radius on `High`:
`pixelox retouch heal work.pxo --in-place --layer High --at 1402,1188 --radius 4`.
A large radius flattens the texture; keep it small.

### 7. Dodge and burn (optional)

Paint where to lighten or darken into named masks, then dodge or burn the `Low` layer
gently:

```bash
pixelox paint stroke work.pxo --in-place --into dodge --points '1300,1450 1420,1470' --size 160 --flow 15 --color white
pixelox retouch dodge work.pxo --in-place --layer Low --mask dodge --exposure 0.15
pixelox paint stroke work.pxo --in-place --into burn --points '1080,1700 1150,1820' --size 140 --flow 15 --color white
pixelox retouch burn work.pxo --in-place --layer Low --mask burn --exposure 0.15
```

### 8. Check and deliver

```bash
pixelox export work.pxo -o portrait-retouched.jpg --quality 92
pixelox analyze diff portrait.jpg portrait-retouched.jpg --json
pixelox export portrait.jpg -o before-crop.png --region 1150,1200,700x700
pixelox export work.pxo -o after-crop.png --region 1150,1200,700x700
pixelox analyze stats portrait.jpg --rect 1250,1550,40x40 --json
pixelox analyze stats portrait-retouched.jpg --rect 1250,1550,40x40 --json
pixelox verify portrait-retouched.jpg.pixelox.json --json
```

Look at the crops side by side. The cheek's mean color should move by a few levels at most:
evening redness is fine, shifting the skin tone is not. A natural retouch changes a few
percent of the pixels (`changed_percent`); tens of percent means something leaked.

## When it goes wrong

| Symptom | Cause | Fix |
| --- | --- | --- |
| Plastic, waxy skin | Smoothing too strong, or High was blurred | Lower the surface blur; never filter `High` |
| Blotchy tone | Low radius too small, threshold too high | Raise `--radius` a little, lower `--threshold` |
| Halo at the jaw or hairline | The skin mask leaked past the face | Lower `--tolerance`, add `--at` points, `mask contract` |
| A mole or freckle vanished | `--heal` without `--min-redness` | Undo it (`history undo`), then heal with `--min-redness 10` |
| Skin color changed | Hue or saturation edits on skin | Even tone on `Low` instead; never shift skin hue |
| Spots left on dark skin | Low contrast against the skin | Raise `--sensitivity` in steps; check the list, not just the count |
