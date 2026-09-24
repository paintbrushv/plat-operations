-- Migration 009: add days_late to unit_receivables for real delinquency aging buckets.
-- Nullable + backward-compatible: existing rows keep NULL (treated as "aged/unknown").
ALTER TABLE unit_receivables ADD COLUMN days_late INTEGER;
