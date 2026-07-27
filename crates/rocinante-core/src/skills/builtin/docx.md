---
name: docx
description: "Create and edit Word documents (.docx): headings, paragraphs, tables, images, styles, page setup. Use when asked to write a Word document, produce a .docx report or letter, convert markdown or text to Word, or modify an existing .docx."
---

# Word Documents (.docx)

Two routes — pick by the job. Run everything with the `bash` tool; always report the output file path.

1. **Route by need:**
   | Job | Route |
   |---|---|
   | Text/report you can write as markdown | pandoc (step 2) — simplest, use when available |
   | Precise layout, tables from data, editing an existing .docx | python-docx (step 3) |
   | Convert an existing .docx to PDF | `libreoffice --headless --convert-to pdf FILE.docx` |

2. **Markdown → Word with pandoc.** Check `pandoc --version`; if missing use step 3 instead. Write the content as a markdown file (`write` tool), then:
```bash
pandoc report.md -o report.docx
# with a reference style (fonts/margins copied from an existing doc):
pandoc report.md --reference-doc=template.docx -o report.docx
```
   Markdown headings/lists/tables/images all carry over. This is the best route for prose.

3. **python-docx** (`python3 -m pip install python-docx`) — build or edit programmatically:
```python
from docx import Document
from docx.shared import Inches, Pt

doc = Document()                      # or Document("existing.docx") to edit
doc.add_heading("FILL IN TITLE", level=0)
doc.add_heading("Section", level=1)
p = doc.add_paragraph("Body text with ")
p.add_run("bold").bold = True
doc.add_paragraph("A bullet", style="List Bullet")
doc.add_paragraph("A numbered item", style="List Number")

table = doc.add_table(rows=1, cols=3)
table.style = "Light Grid Accent 1"
for cell, head in zip(table.rows[0].cells, ["A", "B", "C"]):
    cell.text = head
row = table.add_row().cells           # one add_row() per data row
row[0].text = "1"

doc.add_picture("figure.png", width=Inches(5.5))
doc.add_page_break()
doc.save("out.docx")
print("saved out.docx")
```
   DataFrame → table: loop `for r in df.itertuples(index=False): cells = table.add_row().cells; ...` — never paste a printed frame as text.

4. **Edit an existing document**: `Document("in.docx")`; iterate `doc.paragraphs` (read `.text`, modify runs) and `doc.tables`; save under a NEW name — never overwrite the user's original.

5. **Verify** before reporting: reopen and count content:
```bash
python3 -c "from docx import Document; d=Document('out.docx'); print(len(d.paragraphs),'paragraphs,',len(d.tables),'tables')"
```

## Rules

- Never overwrite an input document — write `*_edited.docx` or a new name.
- Images must exist on disk before `add_picture` (matplotlib figures: savefig first, Agg backend as always).
- Styles: stick to built-ins ("List Bullet", "List Number", "Light Grid Accent 1", heading levels 0–4); custom styles must already exist in the document or python-docx raises KeyError.
- Legacy `.doc` cannot be written by these tools — produce `.docx`, or convert at the end with `libreoffice --headless --convert-to doc` if truly required.
- Long report from analysis outputs → consider the `quarto` skill (`format: docx`) for a reproducible route; this skill is for direct document construction.
