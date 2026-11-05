-- Each person's own settings for a conversation: pinned, marked unread,
-- muted (DESIGN FLOW-CHAT-PREFS, ENT-ConversationPref, §25 migration 0016).
-- No row is the same as all three false. A change tells that person's web
-- app and App the conversation list changed (FLOW-LIVE). Only adds a table,
-- so older builds keep working.

CREATE TABLE conversation_prefs (
    account_id       integer NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    conversation_id  bigint NOT NULL REFERENCES conversations (id) ON DELETE CASCADE,
    pinned           boolean NOT NULL DEFAULT false,
    marked_unread    boolean NOT NULL DEFAULT false,
    muted            boolean NOT NULL DEFAULT false,
    PRIMARY KEY (account_id, conversation_id)
);

CREATE FUNCTION imbo_conversation_prefs_changed() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    PERFORM imbo_notify(ARRAY[coalesce(NEW.account_id, OLD.account_id)], 'conversations');
    RETURN NULL;
END $$;

CREATE TRIGGER conversation_prefs_events AFTER INSERT OR UPDATE OR DELETE ON conversation_prefs
    FOR EACH ROW EXECUTE FUNCTION imbo_conversation_prefs_changed();
