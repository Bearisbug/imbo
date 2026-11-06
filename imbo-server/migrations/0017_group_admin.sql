-- Group admins, groups anyone can find, and asking to join one (DESIGN
-- FLOW-GROUP-DISCOVER, FLOW-GROUP-APPROVE, FLOW-GROUP-ADMIN, ADR-065, §25
-- migration 0017). A join request goes to the group, not to one person:
-- its to_account is empty, and its owner and admins handle it. Only adds a
-- column and loosens checks, so older builds keep working.

ALTER TABLE chat_groups ADD COLUMN discoverable boolean NOT NULL DEFAULT false;

ALTER TABLE group_members DROP CONSTRAINT group_members_role_check;
ALTER TABLE group_members ADD CONSTRAINT group_members_role_check
    CHECK (role IN ('owner', 'admin', 'member'));

ALTER TABLE requests DROP CONSTRAINT requests_kind_check;
ALTER TABLE requests ADD CONSTRAINT requests_kind_check
    CHECK (kind IN ('contact', 'group', 'join'));
ALTER TABLE requests ALTER COLUMN to_account DROP NOT NULL;
ALTER TABLE requests ADD CONSTRAINT requests_to_account_check
    CHECK ((kind = 'join') = (to_account IS NULL));
-- requests_one_pending can't hold an empty to_account to one.
CREATE UNIQUE INDEX requests_one_pending_join
    ON requests (from_account, group_id)
    WHERE kind = 'join' AND status = 'pending';
CREATE INDEX requests_group ON requests (group_id, status) WHERE kind = 'join';

-- A join request: its sender, and the group's owner and admins.
CREATE OR REPLACE FUNCTION imbo_requests_changed() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    r requests := coalesce(NEW, OLD);
BEGIN
    IF r.kind = 'join' THEN
        PERFORM imbo_notify(r.from_account || array(
            SELECT account_id FROM group_members
            WHERE group_id = r.group_id AND role IN ('owner', 'admin')), 'requests');
    ELSE
        PERFORM imbo_notify(ARRAY[r.from_account, r.to_account], 'requests');
    END IF;
    RETURN NULL;
END $$;
