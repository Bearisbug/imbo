-- Invites into and requests to join a group are closed when the group is
-- dissolved. The ones of groups dissolved before that was so still wait;
-- this closes them the same way, telling nobody (DESIGN ENT-Request,
-- §25 migration 0020). Changes rows only; older builds keep working.

UPDATE requests r SET status = 'declined', decided_at = now()
FROM chat_groups g
WHERE g.id = r.group_id AND g.dissolved_at IS NOT NULL AND r.status = 'pending';
