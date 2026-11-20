-- Traffic spike alerts: a per-site threshold (NULL = off), a third subscription kind that uses the
-- report mailbox and switches, and the state of the spike each site is in.
ALTER TABLE site_settings
    ADD COLUMN spike_threshold integer CHECK (spike_threshold >= 1);

ALTER TABLE weekly_report_subscriptions
    DROP CONSTRAINT weekly_report_subscriptions_kind_check,
    ADD CONSTRAINT weekly_report_subscriptions_kind_check
        CHECK (kind IN ('weekly', 'monthly', 'spike'));

-- A spike claim's period_end is the start of the spike, so each recipient gets one email per spike.
ALTER TABLE weekly_report_claims
    DROP CONSTRAINT weekly_report_claims_kind_check,
    ADD CONSTRAINT weekly_report_claims_kind_check
        CHECK (kind IN ('weekly', 'monthly', 'spike'));

-- A row means the site is in a spike. It ends, and the row is deleted, once the current visitors
-- have stayed below the threshold for the cooldown since below_since.
CREATE TABLE traffic_spikes (
    site_id     bigint PRIMARY KEY REFERENCES sites (id),
    started_at  timestamptz NOT NULL,
    below_since timestamptz
);
