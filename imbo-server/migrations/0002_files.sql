-- Attachments (DESIGN §7, §9, ENT-File, ADR-011). Content lives under
-- <data dir>/files/<id>; rows stay after the content is removed so the
-- day's upload total stays right.

CREATE TABLE files (
    id          uuid PRIMARY KEY,
    account_id  integer NOT NULL REFERENCES accounts (id),
    message_id  bigint REFERENCES messages (id),
    name        text NOT NULL,
    size        bigint NOT NULL CHECK (size BETWEEN 0 AND 104857600),
    sha256      bytea NOT NULL CHECK (length(sha256) = 32),
    state       text NOT NULL DEFAULT 'uploading'
                CHECK (state IN ('uploading', 'ready', 'sent', 'discarded', 'expired')),
    created_at  timestamptz NOT NULL DEFAULT now(),
    expires_at  timestamptz NOT NULL
);
CREATE INDEX files_account_time ON files (account_id, created_at);
CREATE INDEX files_state_expiry ON files (state, expires_at);
CREATE INDEX files_message ON files (message_id) WHERE message_id IS NOT NULL;
