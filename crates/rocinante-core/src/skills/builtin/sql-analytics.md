---
name: sql-analytics
description: "Write and optimize analytical SQL — SELECT queries, CTEs, window functions, aggregation — across dialects, and use DuckDB to query large CSV/Parquet/JSON files efficiently. Use when asked to write a SQL query, speed one up, analyze a large data file, or pick between SQL engines."
---

# SQL Analytics

Follow the steps in order. Run everything with the `bash` tool.

1. **Identify the engine.** Ask which database, or detect it (connection strings, docker-compose, config files via `grep`). For a **local file** (CSV/Parquet/JSON) the engine is **DuckDB** — go to step 5 first. If the engine is unknown, write portable ANSI SQL and say which assumptions you made. Dialect differences that break queries: quoting (`"col"` vs `` `col` ``), `LIMIT` vs `TOP`, date arithmetic (`INTERVAL` vs `DATE_ADD` vs `julianday`), integer division, string aggregation (`STRING_AGG`/`GROUP_CONCAT`/`LISTAGG`), `QUALIFY` (DuckDB/BigQuery/Snowflake only).

2. **Write the simplest query that answers the question** — plain `SELECT` … `WHERE` … `GROUP BY`. Only add machinery from step 3 when the simple form cannot express it.

3. **Add structure only as needed:**
   - Multi-stage logic → **CTEs**: `WITH filtered AS (...), derived AS (...) SELECT ... FROM derived`. One named stage per idea. Use CTEs instead of nested subqueries.
   - A row needs context from other rows without collapsing them → **window functions**:
     - latest row per key: `ROW_NUMBER() OVER (PARTITION BY id ORDER BY ts DESC) = 1` (wrap in a CTE, filter in the outer query — `WHERE` cannot see window results).
     - previous-row delta: `value - LAG(value) OVER (PARTITION BY id ORDER BY ts)`.
     - running total: `SUM(x) OVER (PARTITION BY id ORDER BY ts)`.
     - 7-row rolling mean: `AVG(x) OVER (ORDER BY ts ROWS BETWEEN 6 PRECEDING AND CURRENT ROW)`.
     - share of total: `x / SUM(x) OVER ()`.
   - Pivot-style counts → `COUNT(*) FILTER (WHERE cond)` or `SUM(CASE WHEN cond THEN 1 ELSE 0 END)`.
   - "In A but not in B" → `LEFT JOIN b ON ... WHERE b.key IS NULL` or `NOT EXISTS`. Never `NOT IN` on a nullable column (returns no rows when a NULL is present).

4. **If the query is slow, optimize in this order:**
   1. Run `EXPLAIN` (or `EXPLAIN ANALYZE`) and read the plan — do not guess.
   2. Filter earlier; keep predicates sargable: `WHERE date_col >= '2024-01-01'`, never `WHERE YEAR(date_col) = 2024` (a function on the column defeats the index).
   3. Select only the columns needed — no `SELECT *`.
   4. Aggregate before joining when a join would multiply rows.
   5. Replace correlated per-row subqueries with a join or window function.

5. **Local files: DuckDB.** Check it exists with `duckdb --version`; if missing use `python3 -c "import duckdb"` and install with `python3 -m pip install duckdb`.

```bash
# first move on any unknown file — column names, types, min/max, nulls:
duckdb -c "SUMMARIZE SELECT * FROM read_csv_auto('data.csv');"

# query files in place (globs work):
duckdb -c "SELECT col, count(*) FROM read_parquet('data/*.parquet') GROUP BY col;"
duckdb -c "SELECT ... FROM read_json_auto('events.json');"

# convert once, query fast forever:
duckdb -c "COPY (SELECT * FROM read_csv_auto('big.csv')) TO 'big.parquet' (FORMAT PARQUET);"
```

   Python fallback when the CLI is missing:
```bash
python3 -c "import duckdb; print(duckdb.sql(\"SUMMARIZE SELECT * FROM read_csv_auto('data.csv')\"))"
```
   `duckdb.sql("...").df()` hands the result to pandas for plotting or stats. DuckDB handles larger-than-memory data and only reads the Parquet columns/rows the query needs — prefer it over loading big files into pandas.

## Rules

- Run queries with the `bash` tool; put long SQL in a `.sql` file with the `write` tool and run `duckdb -c ".read query.sql"` or pipe it.
- Never invent tables or columns: list them first (`SUMMARIZE`, `DESCRIBE table`, or the engine's catalog) and copy the names exactly.
- Present results as text tables. If a chart is needed, hand the result to pandas/matplotlib with `matplotlib.use("Agg")` and `savefig` — never `plt.show()`.
- If a query errors, read the message — it usually names the bad column or syntax — fix that exact thing and re-run.
- To restructure messy raw data into queryable tables first, call the `skill` tool with `{"name": "data-wrangling"}`.
