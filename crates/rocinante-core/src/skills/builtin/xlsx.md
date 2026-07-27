---
name: xlsx
description: "Create and edit Excel spreadsheets (.xlsx): sheets, formatted cells, number formats, formulas, frozen headers, autofilters, charts, DataFrames to sheets. Use when asked to produce a spreadsheet, export data to Excel, format or add formulas to a workbook, or edit an existing .xlsx."
---

# Excel Spreadsheets (.xlsx)

openpyxl does the work (`python3 -m pip install openpyxl`; pandas uses it as the engine). Run with the `bash` tool; report the output path. Layout rule: one worksheet per table of data plus a small summary sheet — never one giant blob.

1. **DataFrame → workbook (the fast path):**
```python
import pandas as pd
df = pd.read_csv("FILL_IN.csv")
with pd.ExcelWriter("out.xlsx", engine="openpyxl") as writer:
    df.to_excel(writer, sheet_name="data", index=False)
    df.groupby("GROUP_COL").agg(total=("VALUE_COL", "sum")).reset_index() \
      .to_excel(writer, sheet_name="summary", index=False)
    ws = writer.sheets["data"]          # openpyxl worksheet — format below
    ws.freeze_panes = "A2"
    ws.auto_filter.ref = ws.dimensions
```

2. **Formatting essentials** (on any openpyxl worksheet):
```python
from openpyxl.styles import Font
from openpyxl.utils import get_column_letter

for cell in ws[1]:                       # bold header row
    cell.font = Font(bold=True)
for col_idx, width in enumerate(
    (max(len(str(c.value or "")) for c in col) + 2 for col in ws.columns), start=1
):
    ws.column_dimensions[get_column_letter(col_idx)].width = min(width, 40)
```
   Number formats — set on cells, never pre-format values into strings:
   | Content | `cell.number_format` |
   |---|---|
   | Money / 2-dp numbers | `"#,##0.00"` |
   | Percentage (store 0.173) | `"0.0%"` |
   | Date (store `datetime`) | `"yyyy-mm-dd"` |
   Apply to a column: `for c in ws["C"][1:]: c.number_format = "#,##0.00"`.

3. **Formulas** are strings starting with `=`; Excel computes them on open:
```python
last = ws.max_row
ws[f"C{last + 1}"] = f"=SUM(C2:C{last})"
```
   openpyxl does NOT evaluate formulas — reading the cell back gives the formula text, not the value. Say so when reporting; compute totals in Python too when the report needs the number.

4. **Chart** (bar shown; `LineChart` identical shape):
```python
from openpyxl.chart import BarChart, Reference
chart = BarChart()
chart.title = "FILL IN"
data = Reference(ws, min_col=2, min_row=1, max_row=ws.max_row)   # incl. header
cats = Reference(ws, min_col=1, min_row=2, max_row=ws.max_row)
chart.add_data(data, titles_from_data=True)
chart.set_categories(cats)
ws.add_chart(chart, "E2")
```

5. **Edit an existing workbook**: `from openpyxl import load_workbook; wb = load_workbook("in.xlsx")` — sheets via `wb["name"]`, add with `wb.create_sheet("new")`, save under a NEW name.

6. **Verify** before reporting:
```bash
python3 -c "from openpyxl import load_workbook; wb=load_workbook('out.xlsx'); print(wb.sheetnames, [wb[s].dimensions for s in wb.sheetnames])"
```

## Rules

- Numbers go in as numbers, dates as `datetime` objects — never as formatted strings (Excel flags text-numbers and sorting/formulas break). Formatting is `number_format`'s job.
- Never overwrite an input workbook — save `*_edited.xlsx` or a new name.
- Headers in row 1, `freeze_panes = "A2"`, autofilter on — every data sheet, every time.
- Huge data (100k+ rows): write with `pandas.to_excel` directly (openpyxl cell-by-cell is slow), or question whether Parquet/DuckDB (`skill` tool `{"name": "duckdb"}`) serves the need better than Excel.
- Reading messy spreadsheets is the `data-wrangling` skill's job; this one produces clean output.
