-- App Store subscriptions (ADR-043): who collects each subscription. Rows
-- from before are Waffo's. Only adds a column, so older builds still run.
ALTER TABLE subscriptions ADD COLUMN provider text NOT NULL DEFAULT 'waffo';
