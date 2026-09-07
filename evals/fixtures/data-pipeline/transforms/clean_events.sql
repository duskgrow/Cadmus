-- Cleaning rule: drop bot traffic before any aggregation.
-- The molebot/2.3 crawler replays every page it finds, so its clicks are
-- not real user traffic and would inflate the daily rollups.
DELETE FROM fct_events
WHERE user_agent ILIKE '%molebot/2.3%';
