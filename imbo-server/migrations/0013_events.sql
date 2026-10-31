-- Live updates for the web app and the App (DESIGN FLOW-LIVE, API-079,
-- ADR-058, §25 migration 0013). Every change a person would see in a list
-- sends NOTIFY imbo_events '<account id>:<area>' to each account affected;
-- the server passes the area on to that account's connections. Postgres
-- delivers on commit and drops repeats within a transaction. Only adds
-- functions and triggers, so older builds keep working.

CREATE FUNCTION imbo_notify(accounts integer[], area text) RETURNS void
LANGUAGE sql AS $$
    SELECT pg_notify('imbo_events', a || ':' || area) FROM unnest(accounts) AS a WHERE a IS NOT NULL;
$$;

CREATE FUNCTION imbo_group_members(gid integer) RETURNS integer[]
LANGUAGE sql STABLE AS $$
    SELECT coalesce(array_agg(account_id), '{}') FROM group_members WHERE group_id = gid;
$$;

-- A session: its owner's session list, the owner's contacts (they see it
-- under the owner) and the groups it gets messages in.
CREATE FUNCTION imbo_agents_changed() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    r agents := coalesce(NEW, OLD);
BEGIN
    IF r.tool = 'terminal' THEN
        RETURN NULL;
    END IF;
    PERFORM imbo_notify(ARRAY[r.account_id], 'agents');
    PERFORM imbo_notify(array(SELECT account_id FROM contacts WHERE peer_id = r.account_id), 'contacts');
    PERFORM imbo_notify(array(
        SELECT m.account_id FROM member_sessions s JOIN group_members m ON m.group_id = s.group_id
        WHERE s.agent_id = r.id), 'groups');
    RETURN NULL;
END $$;

CREATE TRIGGER agents_events_row AFTER INSERT OR DELETE ON agents
    FOR EACH ROW EXECUTE FUNCTION imbo_agents_changed();
CREATE TRIGGER agents_events_update AFTER UPDATE ON agents
    FOR EACH ROW
    WHEN (OLD.online IS DISTINCT FROM NEW.online
          OR OLD.title IS DISTINCT FROM NEW.title
          OR OLD.custom_title IS DISTINCT FROM NEW.custom_title
          OR OLD.device_id IS DISTINCT FROM NEW.device_id)
    EXECUTE FUNCTION imbo_agents_changed();

-- A message: everyone in its conversation.
CREATE FUNCTION imbo_messages_changed() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    c conversations;
BEGIN
    SELECT * INTO c FROM conversations WHERE id = NEW.conversation_id;
    IF c.kind = 'group' THEN
        PERFORM imbo_notify(imbo_group_members(c.group_id), 'conversations');
    ELSE
        PERFORM imbo_notify(ARRAY[c.account_lo, c.account_hi], 'conversations');
    END IF;
    RETURN NULL;
END $$;

CREATE TRIGGER messages_events AFTER INSERT ON messages
    FOR EACH ROW EXECUTE FUNCTION imbo_messages_changed();

-- A delivery that was made, went through or was read: the receiving
-- account and the sender (the red dot and "waiting for … to come online").
CREATE FUNCTION imbo_deliveries_changed() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    PERFORM imbo_notify(ARRAY[NEW.account_id, (SELECT from_account FROM messages WHERE id = NEW.message_id)],
                        'conversations');
    RETURN NULL;
END $$;

CREATE TRIGGER deliveries_events_insert AFTER INSERT ON deliveries
    FOR EACH ROW EXECUTE FUNCTION imbo_deliveries_changed();
CREATE TRIGGER deliveries_events_update AFTER UPDATE ON deliveries
    FOR EACH ROW WHEN (OLD.state IS DISTINCT FROM NEW.state)
    EXECUTE FUNCTION imbo_deliveries_changed();

-- Contacts are stored both ways round, so each side gets its own row.
CREATE FUNCTION imbo_contacts_changed() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    PERFORM imbo_notify(ARRAY[coalesce(NEW.account_id, OLD.account_id)], 'contacts');
    RETURN NULL;
END $$;

CREATE TRIGGER contacts_events AFTER INSERT OR DELETE ON contacts
    FOR EACH ROW EXECUTE FUNCTION imbo_contacts_changed();

CREATE FUNCTION imbo_requests_changed() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    r requests := coalesce(NEW, OLD);
BEGIN
    PERFORM imbo_notify(ARRAY[r.from_account, r.to_account], 'requests');
    RETURN NULL;
END $$;

CREATE TRIGGER requests_events AFTER INSERT OR UPDATE OR DELETE ON requests
    FOR EACH ROW EXECUTE FUNCTION imbo_requests_changed();

-- Members coming, going or changing role: everyone in the group, and the
-- one who just left.
CREATE FUNCTION imbo_group_members_changed() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    r group_members := coalesce(NEW, OLD);
    who integer[] := imbo_group_members(r.group_id) || r.account_id;
BEGIN
    PERFORM imbo_notify(who, 'groups');
    PERFORM imbo_notify(who, 'conversations');
    RETURN NULL;
END $$;

CREATE TRIGGER group_members_events AFTER INSERT OR UPDATE OR DELETE ON group_members
    FOR EACH ROW EXECUTE FUNCTION imbo_group_members_changed();

CREATE FUNCTION imbo_member_sessions_changed() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    PERFORM imbo_notify(imbo_group_members((coalesce(NEW, OLD)).group_id), 'groups');
    RETURN NULL;
END $$;

CREATE TRIGGER member_sessions_events AFTER INSERT OR DELETE ON member_sessions
    FOR EACH ROW EXECUTE FUNCTION imbo_member_sessions_changed();

CREATE FUNCTION imbo_chat_groups_changed() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    who integer[] := imbo_group_members(NEW.id);
BEGIN
    PERFORM imbo_notify(who, 'groups');
    PERFORM imbo_notify(who, 'conversations');
    RETURN NULL;
END $$;

CREATE TRIGGER chat_groups_events AFTER UPDATE ON chat_groups
    FOR EACH ROW
    WHEN (OLD.name IS DISTINCT FROM NEW.name
          OR OLD.avatar_id IS DISTINCT FROM NEW.avatar_id
          OR OLD.dissolved_at IS DISTINCT FROM NEW.dissolved_at)
    EXECUTE FUNCTION imbo_chat_groups_changed();

-- A new name or picture shows wherever the account does: in contacts'
-- lists, in groups and in conversations.
CREATE FUNCTION imbo_accounts_changed() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    peers integer[] := array(SELECT account_id FROM contacts WHERE peer_id = NEW.id);
    mates integer[] := array(
        SELECT DISTINCT o.account_id FROM group_members m JOIN group_members o ON o.group_id = m.group_id
        WHERE m.account_id = NEW.id AND o.account_id <> NEW.id);
BEGIN
    PERFORM imbo_notify(peers, 'contacts');
    PERFORM imbo_notify(mates, 'groups');
    PERFORM imbo_notify(peers || mates, 'conversations');
    RETURN NULL;
END $$;

CREATE TRIGGER accounts_events AFTER UPDATE ON accounts
    FOR EACH ROW
    WHEN (OLD.name IS DISTINCT FROM NEW.name OR OLD.avatar_id IS DISTINCT FROM NEW.avatar_id)
    EXECUTE FUNCTION imbo_accounts_changed();
