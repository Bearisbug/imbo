-- Imbo schema v1 (DESIGN §7, §9).

CREATE TABLE accounts (
    id          integer PRIMARY KEY CHECK (id BETWEEN 10000000 AND 99999999),
    name        text NOT NULL,
    email       text NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX accounts_email_key ON accounts (lower(email));

CREATE TABLE email_codes (
    id          bigserial PRIMARY KEY,
    email       text NOT NULL,
    code_salt   bytea NOT NULL,
    code_hash   bytea NOT NULL,
    status      text NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'consumed', 'locked')),
    attempts    integer NOT NULL DEFAULT 0 CHECK (attempts BETWEEN 0 AND 5),
    created_at  timestamptz NOT NULL DEFAULT now(),
    expires_at  timestamptz NOT NULL
);
CREATE INDEX email_codes_email ON email_codes (lower(email), id DESC);

CREATE TABLE devices (
    id              uuid PRIMARY KEY,
    account_id      integer NOT NULL REFERENCES accounts (id),
    name            text NOT NULL,
    platform        text NOT NULL,
    client_version  text NOT NULL,
    token_hash      bytea NOT NULL UNIQUE,
    created_at      timestamptz NOT NULL DEFAULT now(),
    last_seen_at    timestamptz,
    revoked_at      timestamptz
);
CREATE INDEX devices_account ON devices (account_id);

CREATE TABLE agents (
    id              uuid PRIMARY KEY,
    account_id      integer NOT NULL REFERENCES accounts (id),
    device_id       uuid REFERENCES devices (id),
    tool            text NOT NULL,
    session_id      text NOT NULL,
    short_id        text NOT NULL,
    title           text NOT NULL,
    online          boolean NOT NULL DEFAULT false,
    created_at      timestamptz NOT NULL DEFAULT now(),
    last_online_at  timestamptz,
    UNIQUE (account_id, tool, session_id)
);
CREATE INDEX agents_device ON agents (device_id) WHERE online;

CREATE TABLE chat_groups (
    id          integer PRIMARY KEY CHECK (id BETWEEN 1000000 AND 9999999),
    name        text NOT NULL,
    created_by  integer NOT NULL REFERENCES accounts (id),
    created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE requests (
    id            bigserial PRIMARY KEY,
    kind          text NOT NULL CHECK (kind IN ('contact', 'group')),
    from_account  integer NOT NULL REFERENCES accounts (id),
    to_account    integer NOT NULL REFERENCES accounts (id),
    group_id      integer REFERENCES chat_groups (id),
    note          text,
    status        text NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'accepted', 'declined')),
    decided_by    text CHECK (decided_by IN ('person', 'agent')),
    created_at    timestamptz NOT NULL DEFAULT now(),
    decided_at    timestamptz,
    CHECK ((kind = 'contact') = (group_id IS NULL))
);
CREATE UNIQUE INDEX requests_one_pending
    ON requests (kind, from_account, to_account, coalesce(group_id, 0))
    WHERE status = 'pending';
CREATE INDEX requests_to ON requests (to_account, status);
CREATE INDEX requests_from ON requests (from_account, created_at);

CREATE TABLE contacts (
    account_id  integer NOT NULL REFERENCES accounts (id),
    peer_id     integer NOT NULL REFERENCES accounts (id),
    created_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (account_id, peer_id),
    CHECK (account_id <> peer_id)
);

CREATE TABLE group_members (
    group_id    integer NOT NULL REFERENCES chat_groups (id),
    account_id  integer NOT NULL REFERENCES accounts (id),
    joined_at   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (group_id, account_id)
);
CREATE INDEX group_members_account ON group_members (account_id);

CREATE TABLE member_sessions (
    group_id    integer NOT NULL,
    account_id  integer NOT NULL,
    agent_id    uuid NOT NULL REFERENCES agents (id),
    PRIMARY KEY (group_id, account_id, agent_id),
    FOREIGN KEY (group_id, account_id) REFERENCES group_members (group_id, account_id) ON DELETE CASCADE
);

CREATE TABLE conversations (
    id          bigserial PRIMARY KEY,
    kind        text NOT NULL CHECK (kind IN ('direct', 'group')),
    account_lo  integer REFERENCES accounts (id),
    account_hi  integer REFERENCES accounts (id),
    group_id    integer UNIQUE REFERENCES chat_groups (id),
    created_at  timestamptz NOT NULL DEFAULT now(),
    UNIQUE (account_lo, account_hi),
    CHECK (
        (kind = 'direct' AND account_lo < account_hi AND group_id IS NULL)
        OR (kind = 'group' AND group_id IS NOT NULL AND account_lo IS NULL AND account_hi IS NULL)
    )
);

CREATE TABLE messages (
    id               bigserial PRIMARY KEY,
    conversation_id  bigint NOT NULL REFERENCES conversations (id),
    from_account     integer NOT NULL REFERENCES accounts (id),
    from_agent_id    uuid REFERENCES agents (id),
    to_agent_id      uuid REFERENCES agents (id),
    body             text NOT NULL,
    client_msg_id    text NOT NULL,
    created_at       timestamptz NOT NULL DEFAULT now(),
    UNIQUE (from_account, client_msg_id)
);
CREATE INDEX messages_conversation_time ON messages (conversation_id, created_at);
CREATE INDEX messages_from_time ON messages (from_account, created_at);

CREATE TABLE deliveries (
    message_id     bigint NOT NULL REFERENCES messages (id),
    agent_id       uuid NOT NULL REFERENCES agents (id),
    account_id     integer NOT NULL REFERENCES accounts (id),
    state          text NOT NULL DEFAULT 'pending' CHECK (state IN ('pending', 'pushed', 'delivered', 'canceled')),
    via            text CHECK (via IN ('inject', 'prompt_hook', 'pull')),
    attempts       integer NOT NULL DEFAULT 0,
    next_push_at   timestamptz NOT NULL DEFAULT now(),
    pushed_at      timestamptz,
    delivered_at   timestamptz,
    canceled_at    timestamptz,
    cancel_reason  text,
    created_at     timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (message_id, agent_id)
);
CREATE INDEX deliveries_agent_state ON deliveries (agent_id, state, message_id);
CREATE INDEX deliveries_pushed ON deliveries (pushed_at) WHERE state = 'pushed';
CREATE INDEX deliveries_due ON deliveries (next_push_at) WHERE state = 'pending';
