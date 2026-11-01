-- Which of an account's sessions each contact may see (DESIGN
-- FLOW-CONTACT-SHARE, RULE-018, ADR-062, §25 migration 0014). The contacts
-- row (account_id, peer_id) says what peer_id sees of account_id's
-- sessions. Contacts that exist now keep seeing everything; contacts added
-- later start with nothing chosen. Only adds a column and a table, so older
-- builds keep working.

ALTER TABLE contacts ADD COLUMN share text NOT NULL DEFAULT 'all'
    CHECK (share IN ('all', 'chosen', 'none'));
ALTER TABLE contacts ALTER COLUMN share SET DEFAULT 'chosen';

-- With share = 'chosen': one chosen session, or one computer and every
-- session on it, including ones opened later.
CREATE TABLE contact_shares (
    id          uuid PRIMARY KEY,
    account_id  integer NOT NULL,
    peer_id     integer NOT NULL,
    agent_id    uuid REFERENCES agents (id) ON DELETE CASCADE,
    device_id   uuid REFERENCES devices (id) ON DELETE CASCADE,
    FOREIGN KEY (account_id, peer_id) REFERENCES contacts (account_id, peer_id) ON DELETE CASCADE,
    CHECK ((agent_id IS NULL) <> (device_id IS NULL))
);
CREATE UNIQUE INDEX contact_shares_agent ON contact_shares (account_id, peer_id, agent_id)
    WHERE agent_id IS NOT NULL;
CREATE UNIQUE INDEX contact_shares_device ON contact_shares (account_id, peer_id, device_id)
    WHERE device_id IS NOT NULL;
CREATE INDEX contact_shares_agent_id ON contact_shares (agent_id);
CREATE INDEX contact_shares_device_id ON contact_shares (device_id);

-- Whether viewer may see the session (RULE-018), for the triggers below.
CREATE FUNCTION imbo_session_visible(owner integer, viewer integer, agent uuid, device uuid) RETURNS boolean
LANGUAGE sql STABLE AS $$
    SELECT EXISTS (
        SELECT 1 FROM contacts sc
        WHERE sc.account_id = owner AND sc.peer_id = viewer
          AND (sc.share = 'all'
               OR (sc.share = 'chosen' AND EXISTS (
                   SELECT 1 FROM contact_shares cs
                   WHERE cs.account_id = owner AND cs.peer_id = viewer
                     AND (cs.agent_id = agent OR cs.device_id = device)))));
$$;

-- A session change reaches only the contacts that may see it.
CREATE OR REPLACE FUNCTION imbo_agents_changed() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    r agents := coalesce(NEW, OLD);
BEGIN
    IF r.tool = 'terminal' THEN
        RETURN NULL;
    END IF;
    PERFORM imbo_notify(ARRAY[r.account_id], 'agents');
    PERFORM imbo_notify(array(
        SELECT account_id FROM contacts
        WHERE peer_id = r.account_id
          AND (imbo_session_visible(r.account_id, account_id, r.id, r.device_id)
               OR imbo_session_visible(r.account_id, account_id, r.id, OLD.device_id))), 'contacts');
    PERFORM imbo_notify(array(
        SELECT m.account_id FROM member_sessions s JOIN group_members m ON m.group_id = s.group_id
        WHERE s.agent_id = r.id), 'groups');
    RETURN NULL;
END $$;

-- A new setting or list: both sides' contact views.
CREATE FUNCTION imbo_shares_changed() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    PERFORM imbo_notify(ARRAY[coalesce(NEW.account_id, OLD.account_id),
                              coalesce(NEW.peer_id, OLD.peer_id)], 'contacts');
    RETURN NULL;
END $$;

CREATE TRIGGER contacts_share_events AFTER UPDATE ON contacts
    FOR EACH ROW WHEN (OLD.share IS DISTINCT FROM NEW.share)
    EXECUTE FUNCTION imbo_shares_changed();
CREATE TRIGGER contact_shares_events AFTER INSERT OR DELETE ON contact_shares
    FOR EACH ROW EXECUTE FUNCTION imbo_shares_changed();
