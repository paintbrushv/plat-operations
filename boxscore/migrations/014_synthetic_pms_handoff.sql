CREATE TABLE IF NOT EXISTS pms_handoff_boundaries (
  property_id TEXT NOT NULL,
  period_id TEXT NOT NULL,
  boundary_json TEXT NOT NULL,
  registered_at TEXT NOT NULL,
  PRIMARY KEY(property_id, period_id),
  FOREIGN KEY(property_id) REFERENCES properties(id),
  FOREIGN KEY(period_id) REFERENCES periods(id)
);

CREATE TABLE IF NOT EXISTS pms_source_revisions (
  id TEXT PRIMARY KEY,
  property_id TEXT NOT NULL,
  period_id TEXT NOT NULL,
  profile_version TEXT NOT NULL,
  source_namespace TEXT NOT NULL,
  source_record_id TEXT NOT NULL,
  revision INTEGER NOT NULL CHECK (revision >= 1),
  effective_date TEXT NOT NULL,
  account_code TEXT NOT NULL,
  account_name TEXT NOT NULL,
  category TEXT NOT NULL,
  amount_cents INTEGER NOT NULL,
  source_file TEXT NOT NULL,
  source_row INTEGER NOT NULL,
  accepted_at TEXT NOT NULL,
  gl_actual_id TEXT NOT NULL UNIQUE,
  UNIQUE(property_id, period_id, source_namespace, source_record_id, revision),
  FOREIGN KEY(property_id) REFERENCES properties(id),
  FOREIGN KEY(period_id) REFERENCES periods(id),
  FOREIGN KEY(gl_actual_id) REFERENCES gl_actuals(id)
);

CREATE INDEX IF NOT EXISTS idx_pms_revisions_scope
  ON pms_source_revisions(property_id, period_id, source_namespace);

CREATE TABLE IF NOT EXISTS pms_synthetic_closes (
  id TEXT PRIMARY KEY,
  property_id TEXT NOT NULL,
  period_id TEXT NOT NULL,
  issued_at TEXT NOT NULL,
  issued_actual_noi_cents INTEGER NOT NULL,
  issued_budget_noi_cents INTEGER NOT NULL,
  before_namespace TEXT NOT NULL,
  after_namespace TEXT NOT NULL,
  accepted_revision_count INTEGER NOT NULL,
  UNIQUE(property_id, period_id),
  FOREIGN KEY(property_id) REFERENCES properties(id),
  FOREIGN KEY(period_id) REFERENCES periods(id)
);
