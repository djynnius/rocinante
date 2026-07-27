---
name: medallion-architecture
description: "Turn a folder of mixed files (spreadsheets, CSV, JSON, Parquet, Word documents) into a governed data lake: bronze inventory, star-schema design with fact and dimension tables, silver layer as Parquet, registered in a DuckLake. Use when asked to build a data lake, apply medallion/bronze/silver/gold layers, create a star schema from files, or organize raw data files for analytics."
---

# Medallion Architecture

Bronze (raw files, untouched) → Silver (clean star-schema Parquet) → Gold (marts, on request). Run everything with the `bash` tool; the whole build lives in one re-runnable script. At every **ASK** gate: ask the user with your recommendation; as a subagent, stop and return the question + recommendation in your report.

1. **Bronze inventory — look at everything, modify nothing.** The raw folder IS the bronze layer; it is never edited. Use `glob` to list files, then peek per type:
```bash
duckdb -c "SUMMARIZE SELECT * FROM read_csv_auto('bronze/FILE.csv');"     # csv
duckdb -c "SUMMARIZE SELECT * FROM read_parquet('bronze/FILE.parquet');"  # parquet
duckdb -c "SUMMARIZE SELECT * FROM read_json_auto('bronze/FILE.json');"   # json
python3 -c "import pandas as pd; x=pd.ExcelFile('bronze/FILE.xlsx'); print(x.sheet_names); print(x.parse(x.sheet_names[0]).head())"
python3 -c "import docx; print('\n'.join(p.text for p in docx.Document('bronze/FILE.docx').paragraphs)[:3000])"   # pip install python-docx
```
   Legacy `.doc`: `libreoffice --headless --convert-to docx FILE.doc` first (fallback `antiword FILE.doc`; neither installed → report the file as unread).
   Produce the inventory table: file, type, rows/sheets, what it appears to contain.

2. **Read the instructions before designing anything.** README/instructions/notes files in the folder are the spec; codebooks and data dictionaries define keys, grain, and coding; Word documents often carry the business context. If no instructions exist, infer the entities and grain from the data — then **ASK** to confirm before building.

3. **Design the star schema** and present it for confirmation:
   - Identify the business process being measured → the **fact table grain**: complete the sentence "one row = one ______" (order line, visit, transaction…). Everything follows from the grain.
   - **Facts**: numeric measures at that grain + foreign keys to the dimensions.
   - **Dimensions**: the descriptive entities (who/what/where/when) — one table each, one row per entity, with a surrogate key. Generate a `dim_date` from the fact's date range.
   - Present as a table (table name, type fact/dim, grain/entity, keys, source bronze files) plus a mermaid `erDiagram`. **ASK** to confirm the schema before building.

4. **Build silver as Parquet** — one re-runnable script, dims before facts:
```sql
-- dims: distinct entities + surrogate keys
CREATE TABLE dim_customer AS
SELECT row_number() OVER (ORDER BY natural_key) AS customer_sk, *
FROM (SELECT DISTINCT cust_id AS natural_key, name, region FROM bronze_orders);

-- fact: join natural keys to surrogate keys, keep measures
CREATE TABLE fact_orders AS
SELECT d.customer_sk, dd.date_sk, o.qty, o.amount
FROM bronze_orders o
JOIN dim_customer d ON o.cust_id = d.natural_key
JOIN dim_date dd    ON o.order_date = dd.date;

COPY dim_customer TO 'silver/dim_customer.parquet' (FORMAT PARQUET);
COPY fact_orders  TO 'silver/fact_orders.parquet'  (FORMAT PARQUET);
```
   Cleaning (types, dedupe, canonical categoricals, real NULLs) happens here — the `data-wrangling` skill has the recipes (`skill` tool, `{"name": "data-wrangling"}`). Print reconciliation counts at every step: bronze rows in, silver rows out, and where every dropped row went. A fact row whose natural key misses a dimension is a finding, not a silent inner-join loss — count and report orphans.

5. **Register the silver layer in a DuckLake** so the tables are queryable, transactional, and versioned — load the `skill` tool with `{"name": "ducklake"}` for the mechanics, then create one lake table per silver Parquet.

6. **Gold, on request**: aggregate marts and reporting views built FROM the lake tables (never from bronze), e.g. monthly revenue by region. Keep them as lake tables or views.

7. **Deliver**: the inventory table, the confirmed schema + mermaid ER diagram, the build script, reconciliation counts per layer, and the DuckLake attach snippet the user can query with.

## Rules

- Bronze is read-only, forever. Every transform is scripted and re-runnable from bronze; no hand-edited intermediates.
- Counts reconcile at every layer boundary; unexplained row loss stops the build.
- ASK gates are mandatory at grain choice (step 3) and schema confirmation — a wrong grain invalidates the whole star.
- Files too big for a peek load: stay in DuckDB (it reads lazily); never pull a huge file into pandas just to look at it.
- Run with `python3` / `duckdb`; install helpers with `python3 -m pip install pandas python-docx openpyxl duckdb`.
- Related skills via the `skill` tool: `{"name": "duckdb"}` (engine details), `{"name": "sql-analytics"}` (query patterns), `{"name": "exploratory-data-analysis"}` (profiling silver tables).
