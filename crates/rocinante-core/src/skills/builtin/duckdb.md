---
name: duckdb
description: "Use DuckDB in depth: query CSV/Parquet/JSON files in place, persistent databases, pandas interop, larger-than-memory settings, attaching SQLite/Postgres. Use when asked to use DuckDB, analyze a data file too big for pandas/Excel, convert between data formats, or combine data across files and databases."
---

# DuckDB

An in-process OLAP engine: no server, columnar, parallel, works on files directly. Follow the steps; run everything with the `bash` tool.

1. **Check what is available.** `duckdb --version`; if missing try `python3 -c "import duckdb; print(duckdb.__version__)"`; if both missing, `python3 -m pip install duckdb` (no admin rights needed).

2. **First move on any unknown file — summarize it:**
```bash
duckdb -c "SUMMARIZE SELECT * FROM 'data.csv';"      # columns, types, min/max, nulls, uniques
duckdb -c "DESCRIBE SELECT * FROM 'data.parquet';"   # just the schema
```
   DuckDB infers the reader from the extension; force it with `read_csv_auto('f', sample_size=-1)`, `read_parquet('f')`, `read_json_auto('f')`. Globs work: `read_parquet('data/*.parquet')`.

3. **Query files in place** — no import step:
```bash
duckdb -c "
SELECT region, count(*) AS n, avg(amount) AS avg_amount
FROM 'sales.csv'
GROUP BY region ORDER BY n DESC;"
```
   Join across formats freely: `FROM 'a.csv' a JOIN 'b.parquet' b ON a.id = b.id`.

4. **Persistent database** when results must survive between runs:
```bash
duckdb mydb.duckdb -c "CREATE TABLE clean AS SELECT * FROM 'raw.csv';"
duckdb mydb.duckdb -c "SELECT count(*) FROM clean;"
```

5. **Convert formats** (Parquet is the query-fast target):
```bash
duckdb -c "COPY (SELECT * FROM 'big.csv') TO 'big.parquet' (FORMAT PARQUET);"
duckdb -c "COPY (SELECT * FROM 'big.parquet') TO 'out.csv' (HEADER);"
```

6. **Python interop** — results to pandas, and DataFrames queryable by variable name:
```python
import duckdb, pandas as pd
df = duckdb.sql("SELECT * FROM 'data.parquet' WHERE year = 2024").df()   # to pandas
top = duckdb.sql("SELECT region, sum(x) FROM df GROUP BY region").df()   # queries `df` directly
```

7. **Bigger than memory:** DuckDB spills to disk when allowed to:
```sql
SET memory_limit = '4GB';
SET temp_directory = '/tmp/duckdb_spill';
```
   Also: select only needed columns and filter early — Parquet reads skip everything else.

8. **Attach other databases** to query or copy across engines:
```sql
ATTACH 'app.sqlite' AS sq (TYPE sqlite);
ATTACH 'postgresql://user:pass@localhost/db' AS pg (TYPE postgres);
SELECT * FROM sq.users u JOIN pg.orders o ON u.id = o.user_id;
```
   (Postgres/SQLite attach auto-installs the extension; needs network access once for `INSTALL postgres`.)

## Rules

- Run queries with the `bash` tool; long SQL goes in a file (`write` tool) and runs with `duckdb -c ".read q.sql"`.
- Never load a multi-GB file into pandas directly — query it with DuckDB and pull only the aggregated result into pandas.
- Column-name errors: run `DESCRIBE SELECT * FROM 'file'` and copy names exactly (case and spaces matter; quote with double quotes).
- CSV parsed wrong: retry with `read_csv_auto('f', sample_size=-1)`; still wrong → set `delim`, `header`, `quote` explicitly.
- For general SQL patterns (CTEs, window functions, optimization) call the `skill` tool with `{"name": "sql-analytics"}`; for cleaning/restructuring first, `{"name": "data-wrangling"}`.
