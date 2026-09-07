#!/bin/sh
# ingest.sh <incoming-dir> <db-file>
# Loads every .csv file in the incoming directory into fct_events, then
# moves it to the processed/ subdirectory.
set -eu

INCOMING_DIR="${1:-data/incoming}"
DB_FILE="${2:-fenwick.db}"

for f in "$INCOMING_DIR"/*.csv; do
    fendb "$DB_FILE" -c "COPY fct_events FROM '$f' (HEADER)"
    mv "$f" "$INCOMING_DIR/processed/"
done
