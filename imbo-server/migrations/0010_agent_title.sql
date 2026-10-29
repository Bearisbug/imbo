-- A name the owner gives a session (ADR-045); shown in place of the tool's
-- title when set. Only adds a column, so older builds still run.
ALTER TABLE agents ADD COLUMN custom_title text;
