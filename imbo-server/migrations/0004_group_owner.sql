-- Group owners, removals and dissolving (DESIGN ADR-027, ADR-028, ADR-029, §25).
-- Only adds columns and a table, so older builds keep working (ADR-024).

ALTER TABLE group_members
    ADD COLUMN role text NOT NULL DEFAULT 'member' CHECK (role IN ('owner', 'member'));
CREATE UNIQUE INDEX group_members_one_owner ON group_members (group_id) WHERE role = 'owner';

ALTER TABLE chat_groups ADD COLUMN dissolved_at timestamptz;

CREATE TABLE group_removals (
    group_id    integer NOT NULL REFERENCES chat_groups (id),
    account_id  integer NOT NULL REFERENCES accounts (id),
    removed_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (group_id, account_id)
);

-- A group nobody is in any more counts as dissolved. Owners of the other
-- groups are filled in at every start (db::fill_group_owners).
UPDATE chat_groups g SET dissolved_at = now()
WHERE NOT EXISTS (SELECT 1 FROM group_members m WHERE m.group_id = g.id);
