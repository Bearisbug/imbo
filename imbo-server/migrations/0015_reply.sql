-- The message each message replies to, in the same conversation (DESIGN
-- FLOW-REPLY, §25 migration 0015). No foreign key: the original may go with
-- its sender's account, and the number stays. Only adds a column, so older
-- builds keep working.

ALTER TABLE messages ADD COLUMN in_reply_to bigint;
