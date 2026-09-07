#!/bin/sh
# run_daily.sh <db-file>
# Prints the daily report, newest day first.
set -eu

DB_FILE="${1:-fenwick.db}"

fendb "$DB_FILE" -c "SELECT * FROM fct_daily_rollups ORDER BY rollup_date DESC"
