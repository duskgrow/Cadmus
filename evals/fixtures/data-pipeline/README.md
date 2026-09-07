# fenwick

fenwick is a small click-analytics data pipeline. Raw click events are
ingested from CSV files, cleaned, aggregated into daily rollups, and
printed as a daily report. All SQL runs through `fendb`, the project's
database CLI, against `fenwick.db`.

## Layout

- `schema/` — table definitions (naming rules in `docs/conventions.md`)
- `transforms/` — SQL transforms run over the raw events
- `scripts/` — shell entry points
- `config/` — pipeline configuration
- `docs/` — conventions and changelog

## Run order

```sh
make ingest     # load raw files from data/incoming into fct_events
make transform  # clean fct_events and build fct_daily_rollups
make report     # print the daily report from fct_daily_rollups
```

Run the targets in this order; each step reads the previous step's
output. Configuration lives in `config/pipeline.toml`.
