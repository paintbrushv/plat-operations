CREATE TABLE IF NOT EXISTS account_mappings (
  id TEXT PRIMARY KEY,
  source_system TEXT NOT NULL,
  property_scope TEXT NOT NULL,
  account_code TEXT NOT NULL,
  account_name TEXT NOT NULL,
  noi_category TEXT NOT NULL,
  confidence_score REAL NOT NULL,
  status TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  UNIQUE(source_system, property_scope, account_code)
);
