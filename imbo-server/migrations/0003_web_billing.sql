-- Web sign-in and Creem billing (DESIGN §7, §9, ENT-WebSession,
-- ENT-Subscription, ENT-BillingEvent, ADR-020, ADR-023).

CREATE TABLE web_sessions (
    id          uuid PRIMARY KEY,
    account_id  integer NOT NULL REFERENCES accounts (id),
    token_hash  bytea NOT NULL UNIQUE,
    expires_at  timestamptz NOT NULL,
    revoked_at  timestamptz
);
CREATE INDEX web_sessions_account ON web_sessions (account_id);

-- One row per Creem subscription; status is Creem's, recorded as received.
CREATE TABLE subscriptions (
    id                text PRIMARY KEY,
    mode              text NOT NULL CHECK (mode IN ('test', 'live')),
    account_id        integer NOT NULL REFERENCES accounts (id),
    customer_id       text NOT NULL,
    plan              text NOT NULL CHECK (plan IN ('monthly', 'yearly')),
    status            text NOT NULL,
    paid_until        timestamptz NOT NULL,
    revoked_at        timestamptz,
    creem_updated_at  timestamptz NOT NULL
);
CREATE INDEX subscriptions_account ON subscriptions (account_id, mode);

-- Every webhook as received. The id is Creem's event id, so a redelivery
-- can't be inserted twice (INV-015). No foreign key on subscription_id:
-- events for subscriptions not tied to an account yet are kept too.
CREATE TABLE billing_events (
    id               text PRIMARY KEY,
    type             text NOT NULL,
    subscription_id  text,
    payload          jsonb NOT NULL,
    received_at      timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX billing_events_subscription ON billing_events (subscription_id);
