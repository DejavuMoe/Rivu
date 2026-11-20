-- Scroll depth goals: a visitor completes one by scrolling to at least `scroll_threshold` percent
-- of a page whose pathname matches `page_path` (same `*` wildcard as page goals).
-- Existing event and page goals keep `scroll_threshold` NULL and stay valid under the new checks.
ALTER TABLE goals ADD COLUMN scroll_threshold smallint;

ALTER TABLE goals DROP CONSTRAINT goals_kind_check;
ALTER TABLE goals ADD CONSTRAINT goals_kind_check CHECK (kind IN ('event', 'page', 'scroll'));

ALTER TABLE goals DROP CONSTRAINT goals_check;
ALTER TABLE goals ADD CONSTRAINT goals_check CHECK (
    (kind = 'event' AND event_name IS NOT NULL AND page_path IS NULL AND scroll_threshold IS NULL)
    OR (kind = 'page' AND page_path IS NOT NULL AND event_name IS NULL AND scroll_threshold IS NULL)
    OR (kind = 'scroll' AND page_path IS NOT NULL AND event_name IS NULL
        AND scroll_threshold BETWEEN 1 AND 100)
);

-- Two scroll goals on the same path with different thresholds can coexist.
DROP INDEX goals_unique;
CREATE UNIQUE INDEX goals_unique
    ON goals (site_id, kind, COALESCE(event_name, page_path), COALESCE(scroll_threshold, 0))
    WHERE deleted_at IS NULL;
