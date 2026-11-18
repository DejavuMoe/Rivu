-- Old date intents have no timezone evidence. Leave their bounds NULL.
ALTER TABLE pending_stats_deletions
    ADD COLUMN timezone text,
    ADD COLUMN utc_start_ms bigint,
    ADD COLUMN utc_end_ms bigint,
    ADD COLUMN rejected_before_write boolean NOT NULL DEFAULT false,
    ADD CONSTRAINT pending_stats_deletions_frozen_range CHECK (
        (timezone IS NULL AND utc_start_ms IS NULL AND utc_end_ms IS NULL)
        OR (scope = 'date_range' AND timezone IS NOT NULL
            AND utc_start_ms IS NOT NULL AND utc_end_ms IS NOT NULL
            AND utc_start_ms <= utc_end_ms)
    );
