#!/usr/bin/env bash
# Seed the isolated Boxscore DEMO database from synthetic data.
# Fully separate from live data: writes only to demo/demo.db via DATABASE_URL.
set -euo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DB="$DIR/demo.db"
URL="sqlite://$DB"
# Prefer the freshly-built local binary over any (possibly stale) PATH install,
# so the demo always seeds + launches the CURRENT schema and screens.
REPO_BIN="$DIR/../target/release/boxscore"
if [ -z "${BOXSCORE_BIN:-}" ] && [ ! -x "$REPO_BIN" ]; then
  echo "▸ Building boxscore (release)…"
  ( cd "$DIR/.." && cargo build --release -q )
fi
BIN="${BOXSCORE_BIN:-$REPO_BIN}"

echo "▸ Generating synthetic portfolio…"
python3 "$DIR/generate_demo.py"

echo "▸ Resetting demo DB ($DB)…"
rm -f "$DB"

echo "▸ Ingesting via the real Boxscore pipeline (isolated DB)…"
DATABASE_URL="$URL" "$BIN" ingest property    --file "$DIR/data/properties.csv"   >/dev/null
DATABASE_URL="$URL" "$BIN" ingest gl-actuals  --file "$DIR/data/gl_actuals.csv"   >/dev/null
DATABASE_URL="$URL" "$BIN" ingest gl-budgets  --file "$DIR/data/gl_budgets.csv"   >/dev/null
DATABASE_URL="$URL" "$BIN" ingest rent-roll   --file "$DIR/data/rent_roll.csv"    >/dev/null
DATABASE_URL="$URL" "$BIN" ingest delinquency --file "$DIR/data/delinquency.csv"  >/dev/null
DATABASE_URL="$URL" "$BIN" ingest leasing     --file "$DIR/data/leasing.csv"      >/dev/null
python3 "$DIR/seed_collections.py" "$DB" "$DIR/data/collections.csv"
python3 "$DIR/seed_unit_receivables.py" "$DB" "$DIR/data/unit_receivables.csv"
python3 "$DIR/seed_unit_leases.py" "$DB" "$DIR/data/unit_leases.csv"
python3 "$DIR/seed_track_record.py" "$DB"
python3 "$DIR/seed_transactions.py" "$DB" "$DIR/data/gl_actuals.csv"

echo "▸ Close-readiness check (latest period)…"
DATABASE_URL="$URL" "$BIN" close-readiness --period 2026-05 2>/dev/null | python3 "$DIR/verify.py" "$DB"

echo ""
echo "✅ Demo seeded. Launch the TUI with:"
echo "   ./demo/run.sh        (or:  DATABASE_URL=\"$URL\" boxscore desk)"
