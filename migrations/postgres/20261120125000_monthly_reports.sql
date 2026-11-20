-- Monthly reports share the account mailbox, the per-site switches and the send claims with the
-- weekly report. `kind` tells them apart; the tables keep their weekly names. Existing rows are
-- weekly, and the default keeps an older process that omits the column on weekly.
ALTER TABLE weekly_report_subscriptions
    ADD COLUMN kind text NOT NULL DEFAULT 'weekly' CHECK (kind IN ('weekly', 'monthly')),
    DROP CONSTRAINT weekly_report_subscriptions_pkey,
    ADD PRIMARY KEY (user_id, site_id, kind);

ALTER TABLE weekly_report_claims
    ADD COLUMN kind text NOT NULL DEFAULT 'weekly' CHECK (kind IN ('weekly', 'monthly')),
    DROP CONSTRAINT weekly_report_claims_pkey,
    ADD PRIMARY KEY (user_id, site_id, kind, period_end);
