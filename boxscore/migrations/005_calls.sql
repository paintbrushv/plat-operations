CREATE TABLE IF NOT EXISTS calls (
  id TEXT PRIMARY KEY,
  property_id TEXT NOT NULL,
  origin_period TEXT NOT NULL,
  call_type TEXT NOT NULL,
  status TEXT NOT NULL,
  made_at TEXT NOT NULL,
  mature_by TEXT NOT NULL,
  confidence REAL,
  payload_json TEXT NOT NULL,
  outcome_json TEXT,
  score REAL,
  outcome_summary TEXT,
  scored_at TEXT,
  source_task_run_id TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  FOREIGN KEY(property_id) REFERENCES properties(id),
  FOREIGN KEY(source_task_run_id) REFERENCES task_runs(id)
);
CREATE INDEX IF NOT EXISTS idx_calls_lookup ON calls (property_id, call_type, status, mature_by);
