-- A direct conversation may have the same account on both sides: the
-- person and their own sessions (ADR-030). The web app's reads count as a
-- way of delivery (API-044).
ALTER TABLE conversations DROP CONSTRAINT conversations_check;
ALTER TABLE conversations ADD CONSTRAINT conversations_check CHECK (
    (kind = 'direct' AND account_lo <= account_hi AND group_id IS NULL)
    OR (kind = 'group' AND group_id IS NOT NULL AND account_lo IS NULL AND account_hi IS NULL)
);
ALTER TABLE deliveries DROP CONSTRAINT deliveries_via_check;
ALTER TABLE deliveries ADD CONSTRAINT deliveries_via_check
    CHECK (via IN ('inject', 'prompt_hook', 'pull', 'web'));
