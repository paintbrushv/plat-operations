#!/usr/bin/env bash
# Launch the Boxscore Close Desk TUI against the isolated DEMO database.
# Safe for screen-sharing: synthetic data only, never touches live boxscore.db.
set -euo pipefail
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DB="$DIR/demo.db"
# Prefer the freshly-built local binary over any (possibly stale) PATH install.
REPO_BIN="$DIR/../target/release/boxscore"
if [ -z "${BOXSCORE_BIN:-}" ] && [ ! -x "$REPO_BIN" ]; then
  ( cd "$DIR/.." && cargo build --release -q )
fi
BIN="${BOXSCORE_BIN:-$REPO_BIN}"
if [ ! -f "$DB" ]; then
  echo "Demo DB not found. Seeding first…"
  "$DIR/seed.sh"
fi
exec env DATABASE_URL="sqlite://$DB" "$BIN" desk
