-- A requested change of a user's sign-in email, waiting for the link mailed to the new address.
-- One row per user: a new request replaces the old one, and confirming or cancelling deletes it.
-- Only the SHA-256 of the link token is stored. The link is valid only while the account still
-- has `old_email`, so a change made in the meantime retires it.
CREATE TABLE email_changes (
    user_id    bigint PRIMARY KEY REFERENCES users (id),
    old_email  citext NOT NULL,
    new_email  citext NOT NULL CHECK (char_length(new_email) BETWEEN 3 AND 320),
    lang       text NOT NULL CHECK (lang IN ('zh', 'en')),
    token_hash bytea NOT NULL CHECK (octet_length(token_hash) = 32),
    expires_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX email_changes_token ON email_changes (token_hash);
CREATE INDEX email_changes_expires ON email_changes (expires_at);
