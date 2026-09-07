-- Raw click events: one row per click, loaded by scripts/ingest.sh.
CREATE TABLE fct_events (
    event_id   BIGINT PRIMARY KEY,
    session_id TEXT NOT NULL,
    user_id    TEXT NOT NULL,
    page_path  TEXT NOT NULL,
    referrer   TEXT,
    user_agent TEXT NOT NULL,
    clicked_at TIMESTAMPTZ NOT NULL
);
