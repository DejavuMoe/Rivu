CREATE TABLE weekly_report_subscriptions (
    user_id      bigint NOT NULL REFERENCES users (id),
    site_id      bigint NOT NULL REFERENCES sites (id),
    email        text,
    lang         text NOT NULL CHECK (lang IN ('zh', 'en')),
    requested_at timestamptz NOT NULL,
    code_hash    bytea CHECK (octet_length(code_hash) = 32),
    attempts     smallint NOT NULL DEFAULT 0 CHECK (attempts BETWEEN 0 AND 5),
    confirmed_at timestamptz,
    PRIMARY KEY (user_id, site_id)
);
CREATE INDEX weekly_report_active_site ON weekly_report_subscriptions (site_id)
    WHERE confirmed_at IS NOT NULL;

-- An attempt is permanent across opt-out and opt-in, even if SMTP never answers.
CREATE TABLE weekly_report_claims (
    user_id    bigint NOT NULL,
    site_id    bigint NOT NULL,
    period_end timestamptz NOT NULL,
    PRIMARY KEY (user_id, site_id, period_end),
    FOREIGN KEY (user_id, site_id) REFERENCES weekly_report_subscriptions (user_id, site_id)
);
