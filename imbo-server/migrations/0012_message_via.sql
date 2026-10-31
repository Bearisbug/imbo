-- Where a person wrote a message they sent themselves (ADR-056): their
-- terminal, the App or the web app; empty on what a session sent and on
-- older messages. The App's reads count as a way of delivery, as the web
-- app's do (API-078). Only adds a column, so older builds still run.
ALTER TABLE messages ADD COLUMN from_via text CHECK (from_via IN ('terminal', 'app', 'web'));
ALTER TABLE deliveries DROP CONSTRAINT deliveries_via_check;
ALTER TABLE deliveries ADD CONSTRAINT deliveries_via_check
    CHECK (via IN ('inject', 'prompt_hook', 'pull', 'web', 'app'));
-- The language the App registered with its push token, for the words the
-- server writes into a notification (ADR-057); empty means English.
ALTER TABLE app_sessions ADD COLUMN locale text CHECK (locale IN ('en', 'zh-Hans'));
