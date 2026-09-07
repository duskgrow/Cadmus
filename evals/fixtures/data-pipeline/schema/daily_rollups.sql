-- Daily rollup of cleaned clicks: one row per page_path per day.
-- Built by transforms/rollup_daily.sql.
CREATE TABLE fct_daily_rollups (
    rollup_date DATE NOT NULL,
    page_path   TEXT NOT NULL,
    clicks      BIGINT NOT NULL,
    uniq_users  BIGINT NOT NULL,
    PRIMARY KEY (rollup_date, page_path)
);
