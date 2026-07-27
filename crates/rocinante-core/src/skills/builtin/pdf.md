---
name: pdf
description: "Produce PDFs: render reports and prose to PDF, convert Word/Excel/PowerPoint files to PDF, build fully programmatic PDFs (invoices, forms), extract text from PDFs. Use when asked to create a PDF, convert a document to PDF, or generate a printable file."
---

# PDF

PDFs are an output format with several producers — pick ONE route from the table, say which and why, then follow only that section. Run with the `bash` tool; report the output path.

1. **Route by the source material:**
   | Source | Route |
   |---|---|
   | Analysis report with code/figures | `quarto` skill: `quarto render report.qmd --to pdf` (first time: `quarto install tinytex`) |
   | Prose you can write as markdown | pandoc (step 2) |
   | HTML you have or can write | weasyprint (step 3) |
   | An existing docx / xlsx / pptx | libreoffice headless (step 4) — the universal converter |
   | Precise programmatic layout (invoice, form, certificate) | reportlab (step 5) |

2. **Markdown → PDF with pandoc** — needs a PDF engine; check and pick:
```bash
pandoc --version || echo NO-PANDOC
pandoc doc.md -o doc.pdf                          # uses pdflatex if installed
pandoc doc.md -o doc.pdf --pdf-engine=typst       # if typst is installed (fast, no LaTeX)
```
   No engine available → write the content as HTML and use step 3, or go through docx (`pandoc doc.md -o doc.docx`) then step 4.

3. **HTML → PDF with weasyprint** (`python3 -m pip install weasyprint`):
```bash
python3 -c "import weasyprint; weasyprint.HTML('in.html').write_pdf('out.pdf')"
```
   CSS controls the layout (`@page { size: A4; margin: 2cm }`); good typography with plain HTML+CSS skills.

4. **Any office file → PDF with LibreOffice** (works for docx, xlsx, pptx, odt):
```bash
soffice --headless --convert-to pdf --outdir . FILE.docx 2>/dev/null ||
libreoffice --headless --convert-to pdf --outdir . FILE.docx
```
   Neither binary present → report that LibreOffice is required for office-file conversion and offer route 2/3 instead.

5. **Programmatic with reportlab** (`python3 -m pip install reportlab`) — flowables for documents:
```python
from reportlab.lib.pagesizes import A4
from reportlab.lib.styles import getSampleStyleSheet
from reportlab.lib.units import cm
from reportlab.platypus import SimpleDocTemplate, Paragraph, Spacer, Table, TableStyle, Image
from reportlab.lib import colors

styles = getSampleStyleSheet()
doc = SimpleDocTemplate("out.pdf", pagesize=A4,
                        leftMargin=2*cm, rightMargin=2*cm, topMargin=2*cm, bottomMargin=2*cm)
story = [
    Paragraph("FILL IN TITLE", styles["Title"]),
    Spacer(1, 12),
    Paragraph("Body text. <b>Bold</b> and <i>italic</i> via mini-HTML.", styles["BodyText"]),
    Spacer(1, 12),
]
table = Table([["Item", "Qty", "Price"], ["Widget", "2", "9.99"]])
table.setStyle(TableStyle([
    ("BACKGROUND", (0, 0), (-1, 0), colors.lightgrey),
    ("GRID", (0, 0), (-1, -1), 0.5, colors.grey),
]))
story += [table, Spacer(1, 12), Image("figure.png", width=14*cm, height=8*cm)]
doc.build(story)
print("saved out.pdf")
```

6. **Extract from an existing PDF** (quick pointer; heavy parsing → `data-wrangling`):
```bash
python3 -m pip install pdfplumber
python3 -c "import pdfplumber; print(pdfplumber.open('in.pdf').pages[0].extract_text()[:2000])"
```

7. **Verify** before reporting:
```bash
file out.pdf                                       # must say "PDF document"
python3 -c "from pypdf import PdfReader; print(len(PdfReader('out.pdf').pages), 'pages')"
```

## Rules

- State the chosen route and why; check its converter exists BEFORE promising the output (`pandoc --version`, `soffice/libreoffice`, `python3 -c "import weasyprint"`).
- Never rasterize text documents into images-as-PDF — text must stay selectable.
- Figures referenced must exist as files first (Agg + savefig).
- Never overwrite an input file; converters keep the source and add `.pdf` alongside.
- PDFs are for delivery, not editing — when the user will iterate on content, produce the docx/qmd source too and say which file is the source of truth.
