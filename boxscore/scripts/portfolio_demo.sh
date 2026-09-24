#!/usr/bin/env bash
set -euo pipefail

export DATABASE_URL="${DATABASE_URL:-sqlite://data/boxscore_portfolio_demo.db?mode=rwc}"
export BOXSCORE_REPORT_DIR="${BOXSCORE_REPORT_DIR:-reports/generated/portfolio-demo}"

DB_PATH="${DATABASE_URL#sqlite://}"
DB_PATH="${DB_PATH%%\?*}"

if [[ "${BOXSCORE_KEEP_DEMO_DB:-0}" != "1" ]]; then
  rm -f "${DB_PATH}" "${DB_PATH}-shm" "${DB_PATH}-wal"
fi

cargo run -- init
cargo run -- demo portfolio
cargo run -- accounts unmapped
cargo run -- gaps list
cargo run -- questions list
cargo run -- capabilities list

echo "Portfolio demo index: ${BOXSCORE_REPORT_DIR}/portfolio-demo-index.md"
