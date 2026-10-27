-- App sign-ins with their push tokens, and deleted account IDs (DESIGN
-- ADR-033, ADR-035, §25). Only adds tables, so older builds keep working.

CREATE TABLE app_sessions (
    id          uuid PRIMARY KEY,
    account_id  integer NOT NULL REFERENCES accounts (id),
    token_hash  bytea NOT NULL UNIQUE,
    platform    text NOT NULL CHECK (platform IN ('ios', 'android')),
    push_token  text UNIQUE,
    expires_at  timestamptz NOT NULL,
    revoked_at  timestamptz
);
CREATE INDEX app_sessions_account ON app_sessions (account_id) WHERE push_token IS NOT NULL;

CREATE TABLE deleted_accounts (
    id          integer PRIMARY KEY,
    deleted_at  timestamptz NOT NULL DEFAULT now()
);
