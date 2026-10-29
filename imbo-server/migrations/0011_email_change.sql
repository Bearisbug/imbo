-- An email-change code carries the account that asked for it (ADR-047); a
-- sign-in code leaves it empty. Only adds a column, so older builds still run.
ALTER TABLE email_codes ADD COLUMN account_id integer REFERENCES accounts (id) ON DELETE CASCADE;
