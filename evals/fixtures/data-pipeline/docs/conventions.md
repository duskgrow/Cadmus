# Conventions

## Table naming

- Fact tables are prefixed `fct_` (e.g. `fct_events`).
- Dimension tables are prefixed `dim_`. The pipeline has none yet.

## Schema files

Each table is defined in its own file under `schema/`:

- `fct_events` is defined in `schema/events.sql`.
- `fct_daily_rollups` is defined in `schema/daily_rollups.sql`.

## Transform files

SQL transforms live in `transforms/` and are named after what they do:
`clean_events.sql` cleans `fct_events`, while `rollup_daily.sql` builds
`fct_daily_rollups` from it.

## Changelog

Every schema or config change gets a dated entry in `docs/changelog.md`
(ISO date, newest first).
