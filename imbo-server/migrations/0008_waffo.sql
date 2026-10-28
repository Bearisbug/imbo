-- Payments moved to Waffo (ADR-042). Its customer page is opened by account
-- ID, so the customer column goes; the ordering column is renamed. An
-- exception to ADR-024: builds before this one read neither column unless a
-- payment platform is configured, and production never configured one.
ALTER TABLE subscriptions RENAME COLUMN creem_updated_at TO provider_updated_at;
ALTER TABLE subscriptions DROP COLUMN customer_id;
