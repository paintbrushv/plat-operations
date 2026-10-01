CREATE TABLE IF NOT EXISTS variance_report_artifacts (
  task_run_id TEXT PRIMARY KEY,
  property_id TEXT NOT NULL,
  period_id TEXT NOT NULL,
  report_path TEXT NOT NULL UNIQUE,
  report_markdown TEXT NOT NULL,
  actual_noi REAL NOT NULL,
  budget_noi REAL NOT NULL,
  issued_at TEXT NOT NULL,
  FOREIGN KEY(task_run_id) REFERENCES task_runs(id),
  FOREIGN KEY(property_id) REFERENCES properties(id),
  FOREIGN KEY(period_id) REFERENCES periods(id)
);

CREATE TABLE IF NOT EXISTS pms_synthetic_close_reports (
  close_id TEXT PRIMARY KEY,
  report_task_run_id TEXT NOT NULL UNIQUE,
  FOREIGN KEY(close_id) REFERENCES pms_synthetic_closes(id),
  FOREIGN KEY(report_task_run_id) REFERENCES variance_report_artifacts(task_run_id)
);
