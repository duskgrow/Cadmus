# Changelog

## 2026-06-14

- Added the `referrer` column to `fct_events` so reports can break down
  traffic by source. Existing rows keep a NULL referrer.

## 2026-05-17

- Raised the ingest `batch_size` default from 2000 to 5000; larger batches
  halved load time in local tests.

## 2026-05-02

- First working pipeline: `make ingest`, `make transform`, `make report`.
