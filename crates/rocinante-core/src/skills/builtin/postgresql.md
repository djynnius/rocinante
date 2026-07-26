---
name: postgresql
description: "Work with PostgreSQL safely: psql, inspecting schemas, backup and restore with pg_dump, users and grants, EXPLAIN ANALYZE and indexing, VACUUM. Use when asked to query, administer, back up, migrate, or speed up a Postgres database, or debug a Postgres connection."
---

# PostgreSQL

Run everything with the `bash` tool through `psql`. Destructive statements follow the transaction rule (Rules section) with no exceptions.

1. **Connect and orient:**
```bash
psql "postgresql://USER:PASS@HOST:5432/DBNAME" -c "\conninfo"
# or: PGPASSWORD=... psql -h HOST -U USER -d DBNAME -c "..."
```
   Inspection meta-commands (run with `-c`):
   | Command | Shows |
   |---|---|
   | `\l` | databases |
   | `\dt` | tables |
   | `\d TABLE` | columns, types, indexes of one table |
   | `\di` | indexes |
   | `\du` | roles/users |
   Add `-x` to psql for readable wide rows. `-t -A` for script-friendly output.

2. **Query from scripts:** put SQL in a file (`write` tool), run `psql "..." -f query.sql`. One-liners with `-c`. Quote carefully: single quotes for SQL strings, double for identifiers.

3. **Backup BEFORE any schema change, migration, or DROP:**
```bash
pg_dump -Fc "postgresql://USER@HOST/DB" -f backup_$(date +%Y%m%d).dump   # compressed, restorable
pg_restore -d "postgresql://USER@HOST/DB" --clean backup.dump            # restore
pg_dump "postgresql://..." --schema-only -f schema.sql                   # plain SQL, reviewable
```
   State the backup file path in your report before proceeding with the change.

4. **Destructive DML — always this shape:**
```sql
BEGIN;
SELECT count(*) FROM orders WHERE status = 'stale';   -- count what will be hit
DELETE FROM orders WHERE status = 'stale';            -- row count must match the SELECT
-- matches expectations? COMMIT;   otherwise: ROLLBACK;
```
   Run it as one `psql -f` script with the COMMIT initially replaced by ROLLBACK for a dry run; re-run with COMMIT once the counts are confirmed.

5. **Performance:**
```sql
EXPLAIN ANALYZE SELECT ...;        -- real timings; read for "Seq Scan" on big tables
CREATE INDEX CONCURRENTLY idx_orders_user ON orders (user_id);   -- no table lock
```
   Index columns that appear in `WHERE`/`JOIN` of slow queries; confirm the plan changed by re-running EXPLAIN ANALYZE. Table stats stale? `VACUUM ANALYZE tablename;`. Find bloated/busy tables: `SELECT relname, n_live_tup, n_dead_tup FROM pg_stat_user_tables ORDER BY n_dead_tup DESC LIMIT 10;`.

6. **Users and grants** (least privilege):
```sql
CREATE ROLE app_ro LOGIN PASSWORD '...';
GRANT CONNECT ON DATABASE mydb TO app_ro;
GRANT USAGE ON SCHEMA public TO app_ro;
GRANT SELECT ON ALL TABLES IN SCHEMA public TO app_ro;
ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT SELECT ON TABLES TO app_ro;
```

7. **Connection troubleshooting, in order:** server reachable (`pg_isready -h HOST -p 5432`) → credentials (`\conninfo` attempt) → `pg_hba.conf` rules (auth errors name it) → database exists (`\l`). Report which step failed.

## Rules

- Every `UPDATE`/`DELETE` runs inside `BEGIN; ... ;` with a preceding count check (step 4). An UPDATE/DELETE without a `WHERE` clause is almost always a bug — stop and confirm with the user.
- `pg_dump` before any `DROP`, `ALTER TABLE`, or migration (step 3) — report the backup path.
- Use `CREATE INDEX CONCURRENTLY` on live databases; plain CREATE INDEX locks writes.
- Never edit `pg_hba.conf`/`postgresql.conf` without copying the original first (`cp f f.bak`); reload with `SELECT pg_reload_conf();` rather than restarting when possible.
- Exploratory analytics on exports/CSVs → the `duckdb` skill (it can even `ATTACH` Postgres read-only); SQL patterns → `{"name": "sql-analytics"}` via the `skill` tool.
