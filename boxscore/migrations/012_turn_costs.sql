CREATE TABLE IF NOT EXISTS turn_costs (
  id TEXT PRIMARY KEY,
  property_id TEXT NOT NULL,
  unit TEXT,
  turn_date TEXT,
  turn_cost_total REAL,
  vacancy_days INTEGER,
  total_turn_impact REAL,
  source_file TEXT NOT NULL,
  source_row INTEGER NOT NULL,
  created_at TEXT NOT NULL,
  FOREIGN KEY(property_id) REFERENCES properties(id)
);
CREATE INDEX IF NOT EXISTS idx_turn_costs_property ON turn_costs(property_id);
