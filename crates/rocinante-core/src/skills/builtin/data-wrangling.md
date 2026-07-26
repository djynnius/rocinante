---
name: data-wrangling
description: "Turn raw or messy data — spreadsheets, exported documents, JSON, ragged CSVs — into clean, SQL-friendly tables and Parquet files for querying. Use when asked to ingest, clean, reshape, normalize, or structure data, or to convert files to Parquet/a queryable form."
---

# Data Wrangling

Follow the steps in order. Run everything with the `bash` tool. Put the whole transform in one script file (via the `write` tool) so it can be re-run — never hand-edit the raw file.

1. **Look at the raw file before parsing anything.**
```bash
head -5 data.csv                      # delimiter, header, quoting
file data.csv                          # encoding hint
python3 -c "import pandas as pd; print(pd.ExcelFile('book.xlsx').sheet_names)"   # sheets
python3 -c "import json,itertools; [print(l[:200]) for l in itertools.islice(open('data.json'),3)]"
```
   Answer these questions and write the answers down:
   - What is one row/record? (the unit of observation)
   - Spreadsheet: title rows above the real header? merged cells? subtotal rows? several tables on one sheet? more than one sheet?
   - CSV: delimiter, encoding (UTF-8 vs latin-1), missing-value markers (`NA`, `NULL`, `-999`, empty), thousands separators, ragged rows?
   - JSON: one object per line or one big array? how deep is the nesting? do arrays inside a record imply a child table?
   - Document exports (docx/pdf): extract the tables to CSV/text first, then treat as above.

2. **Decide the target tables (tidy data).** Rules:
   - One observation per row. One variable per column. One entity type per table.
   - Wide repeated columns (`score_2019, score_2020`) → long: `df.melt(...)` / R `pivot_longer`.
   - Nested JSON → flatten scalars with `pd.json_normalize(records)`; each array-of-objects becomes a child table with the parent's id as a foreign key.
   - Column names snake_case, no spaces or symbols. Real types: dates as dates, `"1,234"` → 1234, `"Yes"/"No"` → boolean. Missing values as real nulls, not sentinel numbers.
   - Every table gets a primary key; invent a row-number surrogate if none exists.

3. **Clean, counting everything.** For each cleaning action print how many rows it touched:
```python
before = len(df)
df["date"] = pd.to_datetime(df["date"], errors="coerce")
print("unparseable dates:", df["date"].isna().sum())
df = df.drop_duplicates(subset=["id"])
print("duplicates dropped:", before - len(df))
```
   - Coerce with `errors="coerce"`, then investigate the failures — do not silently drop rows.
   - Canonicalize categoricals: `df["col"] = df["col"].str.strip().str.lower()`, then map variants to one label.
   - Keep a `col_raw` copy of any column you change destructively, until validated.

4. **Validate.** All of these, and report the results:
   - Row count: source rows in == cleaned rows out + rows removed (each removal explained). If the equation does not balance, find out why before continuing.
   - Primary key unique: `df["id"].is_unique`.
   - Ranges against the codebook/common sense (ages 0–120, dates within study period).
   - If splitting into parent/child tables: every child foreign key exists in the parent.

5. **Convert to Parquet and verify.**
```bash
duckdb -c "COPY (SELECT * FROM read_csv_auto('raw.csv', sample_size=-1)) TO 'clean.parquet' (FORMAT PARQUET);"
```
   or in the script: `df.to_parquet("clean.parquet")`. One Parquet file per tidy table.
   Verify the result:
```bash
duckdb -c "SUMMARIZE SELECT * FROM read_parquet('clean.parquet');"
```
   Check the row count and types match what step 4 established.

6. **Deliver**: the transform script, the Parquet file(s), and a short data note listing — source files, unit of observation per table, every cleaning decision with affected row counts, and known limitations. For analysis on the result, call the `skill` tool with `{"name": "sql-analytics"}` (queries) or `{"name": "exploratory-data-analysis"}` (statistics).

## Rules

- Run code with the `bash` tool. Use `python3` (if missing, try `python`). Install packages with `python3 -m pip install pandas pyarrow openpyxl duckdb` as needed.
- Never modify the raw source files — the script reads raw, writes new files.
- Every dropped or altered row must appear in a printed count. No silent data loss.
- If parsing fails, read the error, look at the exact offending line (`sed -n '123p' file.csv`), fix the parse options for that case, and re-run.
- No plots needed here; if one helps, `matplotlib.use("Agg")` and `savefig` — never `plt.show()`.
