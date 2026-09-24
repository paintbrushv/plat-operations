CREATE TABLE IF NOT EXISTS unit_leases (
  id TEXT PRIMARY KEY,
  property_id TEXT NOT NULL,
  as_of_date TEXT NOT NULL,
  unit_label TEXT NOT NULL,
  resident_code TEXT,
  resident_name TEXT,
  market_rent REAL,
  charge_rent REAL,
  source_file TEXT NOT NULL,
  source_row INTEGER NOT NULL,
  created_at TEXT NOT NULL,
  FOREIGN KEY(property_id) REFERENCES properties(id)
);
CREATE INDEX IF NOT EXISTS idx_unit_leases_lookup ON unit_leases (property_id, unit_label, resident_code, as_of_date);
