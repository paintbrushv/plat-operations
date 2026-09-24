CREATE TABLE IF NOT EXISTS monthly_actuals (
  id TEXT PRIMARY KEY,
  property_id TEXT NOT NULL,
  period TEXT NOT NULL,
  account_code TEXT NOT NULL,
  account_name TEXT,
  amount REAL NOT NULL,
  source_file TEXT NOT NULL,
  created_at TEXT NOT NULL,
  FOREIGN KEY(property_id) REFERENCES properties(id)
);
CREATE INDEX IF NOT EXISTS idx_monthly_actuals_lookup ON monthly_actuals (property_id, account_code, period);
