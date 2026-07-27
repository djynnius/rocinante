---
name: pptx
description: "Create PowerPoint presentations (.pptx): title and bullet slides, images, tables, speaker notes, 16:9 decks. Use when asked to make slides, a deck, a presentation from a report or analysis, or to add slides to an existing .pptx."
---

# PowerPoint Decks (.pptx)

python-pptx (`python3 -m pip install python-pptx`). Run with the `bash` tool; report the output path. Deck discipline: one idea per slide, at most 6 bullets of at most ~10 words — a slide is a signpost, not a document.

1. **Plan the deck first** as a list: slide title + 3–5 bullet fragments each (title, agenda/context, one slide per main point, closing/next steps). Show the outline when the content choices are yours to make.

2. **Deck skeleton:**
```python
from pptx import Presentation
from pptx.util import Inches, Pt

prs = Presentation()                      # or Presentation("existing.pptx") to append
prs.slide_width, prs.slide_height = Inches(13.33), Inches(7.5)   # 16:9

# Title slide — layout 0: title + subtitle
s = prs.slides.add_slide(prs.slide_layouts[0])
s.shapes.title.text = "FILL IN TITLE"
s.placeholders[1].text = "Subtitle · Author · Date"

# Bullet slide — layout 1: title + content
s = prs.slides.add_slide(prs.slide_layouts[1])
s.shapes.title.text = "Section point"
tf = s.placeholders[1].text_frame
tf.paragraphs[0].text = "First bullet"    # first bullet reuses paragraph 0
p = tf.add_paragraph(); p.text = "Second bullet"
p = tf.add_paragraph(); p.text = "Sub-point"; p.level = 1

prs.save("deck.pptx")
print("saved deck.pptx, slides:", len(prs.slides))
```
   Layout table (default template): `[0]` title, `[1]` title+content, `[5]` title only, `[6]` blank. Placeholder indexes vary by layout — when unsure, enumerate: `for ph in s.placeholders: print(ph.placeholder_format.idx, ph.name)`.

3. **Image slide** (figures from the analysis skills drop straight in):
```python
s = prs.slides.add_slide(prs.slide_layouts[5])
s.shapes.title.text = "Results"
s.shapes.add_picture("eda_output/fig.png", Inches(1.5), Inches(1.5), width=Inches(10))
```
   One figure per slide, sized to fill; the title carries the takeaway ("Sales doubled in Q3", not "Chart").

4. **Table slide** (small tables only — big tables belong in a spreadsheet):
```python
rows, cols = len(data) + 1, len(headers)
shape = s.shapes.add_table(rows, cols, Inches(1), Inches(1.5), Inches(11), Inches(0.8 * rows))
table = shape.table
for j, h in enumerate(headers):
    table.cell(0, j).text = str(h)
for i, row in enumerate(data, start=1):
    for j, v in enumerate(row):
        table.cell(i, j).text = str(v)
```

5. **Speaker notes** carry the prose the slide leaves out:
```python
s.notes_slide.notes_text_frame.text = "Full talking points for this slide."
```

6. **Verify** before reporting:
```bash
python3 -c "from pptx import Presentation; print(len(Presentation('deck.pptx').slides), 'slides')"
```

## Rules

- Never paste document prose onto slides — a deck from a report is a REWRITE into signposts; the prose goes into speaker notes.
- Figures must exist as image files first (Agg + savefig, as ever); never describe a chart in bullets when the PNG exists.
- Appending to an existing deck: `Presentation("in.pptx")` picks up its layouts/branding; save under a NEW name.
- Custom-branded templates: open the template as the starting Presentation rather than rebuilding the branding by hand.
- Deck to PDF for distribution: `libreoffice --headless --convert-to pdf deck.pptx` (see the `pdf` skill).
