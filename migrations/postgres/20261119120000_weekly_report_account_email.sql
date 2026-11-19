-- The mailbox is confirmed once per account by a link; each site is then a plain on/off switch.
CREATE TABLE weekly_report_emails (
    user_id          bigint PRIMARY KEY REFERENCES users (id),
    -- The address the pending link was sent to, or the address that was confirmed.
    email            text NOT NULL,
    lang             text NOT NULL CHECK (lang IN ('zh', 'en')),
    requested_at     timestamptz,
    token_hash       bytea CHECK (octet_length(token_hash) = 32),
    token_expires_at timestamptz,
    confirmed_at     timestamptz,
    CHECK ((token_hash IS NULL) = (token_expires_at IS NULL)),
    CHECK (token_hash IS NULL OR confirmed_at IS NULL)
);
CREATE UNIQUE INDEX weekly_report_email_token ON weekly_report_emails (token_hash)
    WHERE token_hash IS NOT NULL;

-- Accounts with an active subscription under the per-site code flow already proved their mailbox.
INSERT INTO weekly_report_emails (user_id, email, lang, confirmed_at)
SELECT DISTINCT ON (w.user_id) w.user_id, w.email, w.lang, w.confirmed_at
FROM weekly_report_subscriptions w
JOIN users u ON u.id = w.user_id AND u.email::text = w.email
WHERE w.confirmed_at IS NOT NULL
ORDER BY w.user_id, w.confirmed_at DESC;

-- A row now means "on". Pending requests, switched-off rows and stale addresses are dropped.
DELETE FROM weekly_report_subscriptions w
WHERE w.confirmed_at IS NULL
   OR NOT EXISTS (SELECT FROM users u WHERE u.id = w.user_id AND u.email::text = w.email);

DROP INDEX weekly_report_active_site;
ALTER TABLE weekly_report_subscriptions
    DROP COLUMN requested_at,
    DROP COLUMN code_hash,
    DROP COLUMN attempts,
    ALTER COLUMN email SET NOT NULL,
    ALTER COLUMN confirmed_at SET NOT NULL;
CREATE INDEX weekly_report_subscriptions_site ON weekly_report_subscriptions (site_id);
