-- The message each message replies to, in the same conversation, and the
-- passage of it the reply picked, if one (DESIGN FLOW-REPLY, §25 migration
-- 0015). No foreign key: the original may go with its sender's account, and
-- the number stays. Only adds columns, so older builds keep working.

ALTER TABLE messages ADD COLUMN quote bigint;
ALTER TABLE messages ADD COLUMN quote_text text;
