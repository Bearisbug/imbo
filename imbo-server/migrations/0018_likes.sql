-- A 👍 someone put on a message to say they saw it (DESIGN FLOW-LIKE,
-- ENT-Like, §25 migration 0018). One per person per message. Adding or
-- removing one tells everyone in the conversation its likes changed
-- (FLOW-LIVE). Only adds a table, so older builds keep working.

CREATE TABLE message_likes (
    message_id  bigint NOT NULL REFERENCES messages (id) ON DELETE CASCADE,
    account_id  integer NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    created_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (message_id, account_id)
);

CREATE FUNCTION imbo_message_likes_changed() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    PERFORM imbo_notify(
        CASE WHEN cv.group_id IS NULL THEN ARRAY[cv.account_lo, cv.account_hi]
             ELSE imbo_group_members(cv.group_id) END,
        'likes')
    FROM messages m JOIN conversations cv ON cv.id = m.conversation_id
    WHERE m.id = coalesce(NEW.message_id, OLD.message_id);
    RETURN NULL;
END $$;

CREATE TRIGGER message_likes_events AFTER INSERT OR DELETE ON message_likes
    FOR EACH ROW EXECUTE FUNCTION imbo_message_likes_changed();
