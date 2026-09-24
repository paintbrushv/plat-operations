CREATE TABLE IF NOT EXISTS collection_snapshots (
  id TEXT PRIMARY KEY,
  property_id TEXT NOT NULL,
  as_of_date TEXT NOT NULL,
  total_delinquent REAL NOT NULL,
  delinquent_units INTEGER NOT NULL,
  high_risk_units INTEGER NOT NULL,
  total_opportunity REAL NOT NULL,
  pricing_opportunity REAL NOT NULL,
  missed_fee_total REAL NOT NULL,
  avg_on_time_pct REAL NOT NULL,
  source_file TEXT NOT NULL,
  source_row INTEGER NOT NULL,
  created_at TEXT NOT NULL,
  FOREIGN KEY(property_id) REFERENCES properties(id)
);
