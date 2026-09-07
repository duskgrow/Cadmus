# fenwick pipeline. Run order: ingest -> transform -> report.

DB         := fenwick.db
INGEST_DIR := data/incoming

.PHONY: ingest transform report

ingest:
	scripts/ingest.sh $(INGEST_DIR) $(DB)

transform:
	fendb $(DB) -f transforms/clean_events.sql
	fendb $(DB) -f transforms/rollup_daily.sql

report:
	scripts/run_daily.sh $(DB)
