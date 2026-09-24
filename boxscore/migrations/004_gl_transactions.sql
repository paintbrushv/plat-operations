CREATE TABLE IF NOT EXISTS gl_transactions (
  id TEXT PRIMARY KEY,
  property_id TEXT NOT NULL,
  entity_code TEXT NOT NULL,
  account_code TEXT NOT NULL,
  txn_date TEXT,
  period TEXT NOT NULL,
  payee TEXT NOT NULL DEFAULT '',
  is_resident INTEGER NOT NULL DEFAULT 0,
  control TEXT,
  reference TEXT,
  amount REAL NOT NULL,
  remarks TEXT,
  source_file TEXT NOT NULL,
  source_row INTEGER NOT NULL,
  created_at TEXT NOT NULL,
  FOREIGN KEY(property_id) REFERENCES properties(id)
);
CREATE INDEX IF NOT EXISTS idx_gl_txn_lookup ON gl_transactions(property_id, account_code, period);
CREATE INDEX IF NOT EXISTS idx_gl_txn_payee ON gl_transactions(payee);
CREATE INDEX IF NOT EXISTS idx_gl_txn_source ON gl_transactions(source_file);
