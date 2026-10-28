-- Reports, blocks, bans and avatars (DESIGN §7, §25 migration 0007,
-- ADR-037–041). Only adds tables and columns, so older builds keep working.

CREATE TABLE avatars (
    id          uuid PRIMARY KEY,
    jpeg        bytea NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now()
);

ALTER TABLE accounts
    ADD COLUMN avatar_id uuid UNIQUE REFERENCES avatars (id) ON DELETE SET NULL,
    ADD COLUMN banned_at timestamptz;
ALTER TABLE chat_groups
    ADD COLUMN avatar_id uuid UNIQUE REFERENCES avatars (id) ON DELETE SET NULL;

CREATE TABLE blocks (
    account_id  integer NOT NULL REFERENCES accounts (id),
    blocked_id  integer NOT NULL REFERENCES accounts (id),
    created_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (account_id, blocked_id),
    CHECK (account_id <> blocked_id)
);
CREATE INDEX blocks_blocked ON blocks (blocked_id);

-- Account foreign keys don't cascade: reports outlive a deleted account.
CREATE TABLE reports (
    id           bigserial PRIMARY KEY,
    reporter_id  integer NOT NULL REFERENCES accounts (id),
    target_id    integer NOT NULL REFERENCES accounts (id),
    group_id     integer REFERENCES chat_groups (id),
    reason       text NOT NULL CHECK (reason IN ('spam', 'harassment', 'inappropriate_avatar', 'other')),
    note         text,
    snapshot     jsonb NOT NULL,
    status       text NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'actioned', 'dismissed')),
    action       text CHECK (action IN ('ban', 'remove_avatar')),
    resolved_by  integer REFERENCES accounts (id),
    created_at   timestamptz NOT NULL DEFAULT now(),
    resolved_at  timestamptz
);
CREATE UNIQUE INDEX reports_one_open ON reports (reporter_id, target_id) WHERE status = 'open';
CREATE INDEX reports_status_time ON reports (status, created_at);
