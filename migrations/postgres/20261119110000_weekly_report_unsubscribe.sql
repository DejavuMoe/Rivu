-- Preserve delivery deduplication even after a member's subscription is removed.
ALTER TABLE weekly_report_claims
    DROP CONSTRAINT weekly_report_claims_user_id_site_id_fkey,
    ADD FOREIGN KEY (user_id) REFERENCES users (id),
    ADD FOREIGN KEY (site_id) REFERENCES sites (id),
    ADD COLUMN confirmed_at timestamptz,
    ADD COLUMN unsubscribe_hash bytea CHECK (octet_length(unsubscribe_hash) = 32);

CREATE UNIQUE INDEX weekly_report_unsubscribe_hash ON weekly_report_claims (unsubscribe_hash)
    WHERE unsubscribe_hash IS NOT NULL;

-- Older versions kept subscriptions after access was revoked.
DELETE FROM weekly_report_subscriptions w USING sites s
    WHERE s.id = w.site_id
      AND NOT EXISTS (
          SELECT FROM memberships m
          WHERE m.organization_id = s.organization_id AND m.user_id = w.user_id AND m.deleted_at IS NULL
      )
      AND NOT EXISTS (
          SELECT FROM site_memberships g
          WHERE g.site_id = w.site_id AND g.user_id = w.user_id AND g.deleted_at IS NULL
      );
