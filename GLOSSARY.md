# pg-studio

A launcher and companion for Drizzle Studio: manages per-project database sessions across PostgreSQL, SQLite, Cloudflare D1, Turso, and MySQL.

## Language

**Detached session**:
A running studio/SSH process pair that outlives pg-studio because shutdown disowned it; re-attached by pid on next startup or terminated as stale.
_Avoid_: orphaned session, background session
