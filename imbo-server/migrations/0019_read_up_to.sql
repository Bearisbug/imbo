-- How far each person has read each conversation: the red dot in the web
-- app and the App counts what came after (DESIGN REQ-104,
-- ENT-ConversationPref, §25 migration 0019). No row, or 0, is nothing read.
-- Adds a column and an index and fills the column in; older builds never
-- read it and keep working.

ALTER TABLE conversation_prefs ADD COLUMN read_up_to bigint NOT NULL DEFAULT 0;

-- The count walks a conversation's messages from that message on.
CREATE INDEX messages_conversation_id ON messages (conversation_id, id);

-- What was sent before this counts as read, so nobody finds their old
-- chats full of red dots; what waits in an inbox still counts, by its
-- delivery.
INSERT INTO conversation_prefs (account_id, conversation_id, read_up_to)
SELECT who.account_id, who.conversation_id, last.id
FROM (
    SELECT account_lo AS account_id, id AS conversation_id FROM conversations WHERE kind = 'direct'
    UNION
    SELECT account_hi, id FROM conversations WHERE kind = 'direct'
    UNION
    SELECT gm.account_id, cv.id FROM group_members gm JOIN conversations cv ON cv.group_id = gm.group_id
) who
CROSS JOIN LATERAL (SELECT max(m.id) AS id FROM messages m WHERE m.conversation_id = who.conversation_id) last
WHERE last.id IS NOT NULL
ON CONFLICT (account_id, conversation_id) DO UPDATE SET read_up_to = EXCLUDED.read_up_to;
