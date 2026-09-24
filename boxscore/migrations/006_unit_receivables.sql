CREATE TABLE IF NOT EXISTS unit_receivables (
  id TEXT PRIMARY KEY,
  property_id TEXT NOT NULL,
  as_of_date TEXT NOT NULL,
  resident_code TEXT NOT NULL,
  resident_name TEXT,
  resident_status TEXT,
  total_delinquent REAL NOT NULL,
  current_owed REAL,
  source_file TEXT NOT NULL,
  source_row INTEGER NOT NULL,
  created_at TEXT NOT NULL,
  FOREIGN KEY(property_id) REFERENCES properties(id)
);
CREATE INDEX IF NOT EXISTS idx_unit_receivables_lookup ON unit_receivables (property_id, resident_code, as_of_date);
