CREATE TABLE IF NOT EXISTS unit_pnl (
  id TEXT PRIMARY KEY,
  property_id TEXT NOT NULL,
  unit TEXT NOT NULL,
  period TEXT,
  total_income REAL,
  direct_expense REAL,
  allocated_expense REAL,
  noi REAL,
  source_file TEXT NOT NULL,
  source_row INTEGER NOT NULL,
  created_at TEXT NOT NULL,
  FOREIGN KEY(property_id) REFERENCES properties(id)
);
CREATE INDEX IF NOT EXISTS idx_unit_pnl_property_unit ON unit_pnl(property_id, unit);
