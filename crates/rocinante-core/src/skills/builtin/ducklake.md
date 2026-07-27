---
name: ducklake
description: "Use DuckLake — DuckDB's lakehouse format: a SQL catalog plus Parquet data files with ACID transactions, snapshots, and time travel. Use when asked to create or query a DuckLake, make Parquet tables versioned/transactional/shareable, set up a local lakehouse, or time-travel table history."
---

# DuckLake

A DuckLake = one catalog database (metadata) + a directory of Parquet files (data). Plain files gain ACID transactions, snapshots, time travel, and schema evolution. Verified against DuckLake v1.0.

1. **Version check first** — DuckLake is newer than most models' training data, so trust the installed reality over memory:
```bash
duckdb -c "INSTALL ducklake; SELECT extension_name, installed_version FROM duckdb_extensions() WHERE extension_name='ducklake';"
```
   If any snippet below errors on syntax, fetch the current docs with the `web-research` skill (`https://ducklake.select/docs`) instead of guessing variations.

2. **Create / open a lake** (ATTACH auto-loads the extension after INSTALL):
```bash
mkdir -p lake_data          # DATA_PATH must already exist
duckdb -c "
ATTACH 'ducklake:my_lake.ducklake' AS lake (DATA_PATH 'lake_data/');
USE lake;
SHOW TABLES;"
```
   `my_lake.ducklake` is the catalog file; `lake_data/` holds the Parquet. Both as relative paths. Re-attaching the same pair reopens the same lake from any process — CLI, Python, another agent.

3. **Create and load tables** (e.g. registering a silver layer):
```sql
CREATE TABLE lake.dim_customer AS SELECT * FROM read_parquet('silver/dim_customer.parquet');
CREATE TABLE lake.fact_orders  AS SELECT * FROM read_parquet('silver/fact_orders.parquet');
```
   Inserts, updates, and deletes are transactional like any database table:
```sql
BEGIN; DELETE FROM lake.fact_orders WHERE amount < 0; COMMIT;
```

4. **Query** — ordinary SQL, from anywhere:
```bash
duckdb -c "ATTACH 'ducklake:my_lake.ducklake' AS lake; SELECT count(*) FROM lake.fact_orders;"
python3 -c "import duckdb; duckdb.sql(\"ATTACH 'ducklake:my_lake.ducklake' AS lake\"); print(duckdb.sql('FROM lake.fact_orders LIMIT 5'))"
```

5. **Snapshots and time travel** — every committed change is a snapshot:
```sql
FROM lake.snapshots();                                   -- list versions + timestamps
SELECT * FROM lake.fact_orders AT (VERSION => 1);        -- read the table as of version 1
```
   Compare before/after an update by querying two versions; recovery from a bad write = read the earlier version and rebuild.

6. **Schema evolution**: `ALTER TABLE lake.t ADD COLUMN c INTEGER;` works without rewriting data; old snapshots keep the old schema.

## Rules

- The catalog file is the source of truth: never hand-edit, move, or delete files inside `DATA_PATH` — orphaning data files corrupts the lake. All changes go through SQL.
- Copy the catalog file (`cp my_lake.ducklake my_lake.ducklake.bak`) before destructive operations; it is small and restores instantly.
- One local writer at a time with a file catalog; concurrent writers need the catalog in a real database (PostgreSQL/MySQL/SQLite server) — report this rather than improvising.
- Time travel reads are cheap — prefer `AT (VERSION => n)` over keeping manual backup copies of tables.
- Related skills via the `skill` tool: `{"name": "duckdb"}` (engine, file querying), `{"name": "medallion-architecture"}` (what to put in the lake and why).
