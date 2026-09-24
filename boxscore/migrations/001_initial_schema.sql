CREATE TABLE IF NOT EXISTS properties (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL UNIQUE,
  market TEXT NOT NULL,
  unit_count INTEGER NOT NULL,
  owner_entity TEXT NOT NULL,
  property_manager TEXT NOT NULL,
  created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS periods (
  id TEXT PRIMARY KEY,
  year INTEGER NOT NULL,
  month INTEGER NOT NULL,
  label TEXT NOT NULL UNIQUE
);

CREATE TABLE IF NOT EXISTS gl_actuals (
  id TEXT PRIMARY KEY,
  property_id TEXT NOT NULL,
  period_id TEXT NOT NULL,
  account_code TEXT NOT NULL,
  account_name TEXT NOT NULL,
  category TEXT NOT NULL,
  amount REAL NOT NULL,
  source_file TEXT NOT NULL,
  source_row INTEGER NOT NULL,
  created_at TEXT NOT NULL,
  FOREIGN KEY(property_id) REFERENCES properties(id),
  FOREIGN KEY(period_id) REFERENCES periods(id)
);

CREATE TABLE IF NOT EXISTS gl_budgets (
  id TEXT PRIMARY KEY,
  property_id TEXT NOT NULL,
  period_id TEXT NOT NULL,
  account_code TEXT NOT NULL,
  account_name TEXT NOT NULL,
  category TEXT NOT NULL,
  amount REAL NOT NULL,
  source_file TEXT NOT NULL,
  source_row INTEGER NOT NULL,
  created_at TEXT NOT NULL,
  FOREIGN KEY(property_id) REFERENCES properties(id),
  FOREIGN KEY(period_id) REFERENCES periods(id)
);

CREATE TABLE IF NOT EXISTS rent_roll_snapshots (
  id TEXT PRIMARY KEY,
  property_id TEXT NOT NULL,
  as_of_date TEXT NOT NULL,
  occupied_units INTEGER NOT NULL,
  vacant_units INTEGER NOT NULL,
  leased_units INTEGER NOT NULL,
  notice_units INTEGER NOT NULL,
  down_units INTEGER NOT NULL,
  market_rent_total REAL NOT NULL,
  in_place_rent_total REAL NOT NULL,
  source_file TEXT NOT NULL,
  source_row INTEGER NOT NULL,
  created_at TEXT NOT NULL,
  FOREIGN KEY(property_id) REFERENCES properties(id)
);

CREATE TABLE IF NOT EXISTS delinquency_snapshots (
  id TEXT PRIMARY KEY,
  property_id TEXT NOT NULL,
  as_of_date TEXT NOT NULL,
  delinquent_amount REAL NOT NULL,
  delinquent_units INTEGER NOT NULL,
  prepaid_amount REAL NOT NULL,
  source_file TEXT NOT NULL,
  source_row INTEGER NOT NULL,
  created_at TEXT NOT NULL,
  FOREIGN KEY(property_id) REFERENCES properties(id)
);

CREATE TABLE IF NOT EXISTS leasing_snapshots (
  id TEXT PRIMARY KEY,
  property_id TEXT NOT NULL,
  as_of_date TEXT NOT NULL,
  leads INTEGER NOT NULL,
  tours INTEGER NOT NULL,
  applications INTEGER NOT NULL,
  approvals INTEGER NOT NULL,
  move_ins INTEGER NOT NULL,
  move_outs INTEGER NOT NULL,
  concessions_amount REAL NOT NULL,
  source_file TEXT NOT NULL,
  source_row INTEGER NOT NULL,
  created_at TEXT NOT NULL,
  FOREIGN KEY(property_id) REFERENCES properties(id)
);

CREATE TABLE IF NOT EXISTS task_runs (
  id TEXT PRIMARY KEY,
  task_type TEXT NOT NULL,
  user_prompt TEXT NOT NULL,
  status TEXT NOT NULL,
  confidence_score REAL,
  summary TEXT,
  started_at TEXT NOT NULL,
  completed_at TEXT
);

CREATE TABLE IF NOT EXISTS tool_runs (
  id TEXT PRIMARY KEY,
  task_run_id TEXT NOT NULL,
  tool_name TEXT NOT NULL,
  input_json TEXT NOT NULL,
  output_json TEXT,
  success INTEGER NOT NULL,
  error_message TEXT,
  started_at TEXT NOT NULL,
  completed_at TEXT,
  FOREIGN KEY(task_run_id) REFERENCES task_runs(id)
);

CREATE TABLE IF NOT EXISTS evidence_items (
  id TEXT PRIMARY KEY,
  task_run_id TEXT NOT NULL,
  source_type TEXT NOT NULL,
  source_table TEXT NOT NULL,
  source_id TEXT,
  source_file TEXT,
  source_row INTEGER,
  claim TEXT NOT NULL,
  created_at TEXT NOT NULL,
  FOREIGN KEY(task_run_id) REFERENCES task_runs(id)
);

CREATE TABLE IF NOT EXISTS gaps (
  id TEXT PRIMARY KEY,
  task_run_id TEXT NOT NULL,
  gap_type TEXT NOT NULL,
  severity TEXT NOT NULL,
  description TEXT NOT NULL,
  why_it_matters TEXT NOT NULL,
  proposed_resolution TEXT NOT NULL,
  status TEXT NOT NULL,
  created_at TEXT NOT NULL,
  resolved_at TEXT,
  FOREIGN KEY(task_run_id) REFERENCES task_runs(id)
);

CREATE TABLE IF NOT EXISTS operator_questions (
  id TEXT PRIMARY KEY,
  task_run_id TEXT NOT NULL,
  question TEXT NOT NULL,
  reason TEXT NOT NULL,
  priority INTEGER NOT NULL,
  status TEXT NOT NULL,
  answer TEXT,
  created_at TEXT NOT NULL,
  answered_at TEXT,
  FOREIGN KEY(task_run_id) REFERENCES task_runs(id)
);

CREATE TABLE IF NOT EXISTS capability_backlog (
  id TEXT PRIMARY KEY,
  title TEXT NOT NULL UNIQUE,
  description TEXT NOT NULL,
  trigger_reason TEXT NOT NULL,
  expected_value TEXT NOT NULL,
  implementation_hint TEXT NOT NULL,
  priority INTEGER NOT NULL,
  status TEXT NOT NULL,
  created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS memories (
  id TEXT PRIMARY KEY,
  memory_type TEXT NOT NULL,
  scope TEXT NOT NULL,
  key TEXT NOT NULL,
  value TEXT NOT NULL,
  confidence_score REAL NOT NULL,
  source_task_run_id TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  UNIQUE(memory_type, scope, key)
);
