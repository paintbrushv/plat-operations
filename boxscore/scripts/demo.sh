#!/usr/bin/env bash
set -euo pipefail

export DATABASE_URL="${DATABASE_URL:-sqlite://data/boxscore_demo.db?mode=rwc}"
export BOXSCORE_REPORT_DIR="${BOXSCORE_REPORT_DIR:-reports/generated}"

rm -f data/boxscore_demo.db data/boxscore_demo.db-shm data/boxscore_demo.db-wal

cargo run -- init
cargo run -- ingest property --file data/sample/properties.csv
cargo run -- ingest gl-actuals --file data/sample/gl_actuals.csv
cargo run -- ingest gl-budgets --file data/sample/gl_budgets.csv
cargo run -- ingest rent-roll --file data/sample/rent_roll_snapshots.csv
cargo run -- ingest delinquency --file data/sample/delinquency_snapshots.csv
cargo run -- ingest leasing --file data/sample/leasing_snapshots.csv
cargo run -- analyze variance --property "Oak Ridge" --period "2026-05"
cargo run -- gaps list
cargo run -- questions list
cargo run -- capabilities list

echo "Report generated under ${BOXSCORE_REPORT_DIR}"
