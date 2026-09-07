-- Aggregate cleaned events into fct_daily_rollups.
-- Day boundaries use the pipeline timezone from config/pipeline.toml.
INSERT INTO fct_daily_rollups (rollup_date, page_path, clicks, uniq_users)
SELECT
    CAST(clicked_at AT TIME ZONE 'Etc/UTC' AS DATE) AS rollup_date,
    page_path,
    COUNT(*) AS clicks,
    COUNT(DISTINCT user_id) AS uniq_users
FROM fct_events
GROUP BY 1, 2;
