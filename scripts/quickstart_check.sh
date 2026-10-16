#!/usr/bin/env bash
# Runs the native-binary commands of QUICKSTART.md (Path B and the sections
# after it) against the synthetic files in docs/quickstart/, in a temporary
# directory, and asserts the key values the document quotes. If a command or
# a number in the quickstart changes, this fails.
#
# Usage: scripts/quickstart_check.sh [path/to/boxscore-exact]
# Default binary: boxscore/target/release/boxscore-exact (build it with
#   cd boxscore && cargo build --locked --release --bin boxscore-exact).
# Needs bash and python3 (standard library only). Leaves nothing behind.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="${1:-$ROOT/boxscore/target/release/boxscore-exact}"
[ -x "$BIN" ] || { echo "binary not found or not executable: $BIN" >&2; exit 1; }
BIN="$(cd "$(dirname "$BIN")" && pwd)/$(basename "$BIN")"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
# Run from a copy of the repository layout the quickstart assumes: the
# commands use docs/quickstart/... relative to the working directory.
mkdir -p "$WORK/docs"
cp -R "$ROOT/docs/quickstart" "$WORK/docs/quickstart"
cd "$WORK"
mkdir bin
ln -s "$BIN" bin/boxscore-exact
export PATH="$WORK/bin:$PATH"
# Never inherit a host's trace settings.
unset BOXSCORE_TRACE_DIR BOXSCORE_TRACE_INPUTS BOXSCORE_MCP_DEBUG

PASS=0
fail() { echo "FAIL: $*" >&2; exit 1; }
# jget PATH < json: print the value at a dotted path (list indexes are numbers).
jget() {
  python3 -I -c '
import json, sys
d = json.load(sys.stdin)
for k in sys.argv[1].split("."):
    d = d[int(k)] if isinstance(d, list) else d[k]
print(json.dumps(d) if isinstance(d, (dict, list, bool)) or d is None else d)
' "$1"
}
expect() { # expect LABEL ACTUAL EXPECTED
  if [ "$2" = "$3" ]; then
    PASS=$((PASS + 1))
    echo "ok   $1 = $2"
  else
    fail "$1: expected '$3', got '$2'"
  fi
}
expect_match() { # expect_match LABEL VALUE REGEX
  if [[ "$2" =~ $3 ]]; then
    PASS=$((PASS + 1))
    echo "ok   $1"
  else
    fail "$1: '$2' does not match /$3/"
  fi
}

DB=./exact.sqlite

echo "== describe"
echo '{}' | boxscore-exact call --function describe > describe.json
expect "describe status" "$(jget status < describe.json)" calculated
expect "catalog size" "$(jget result.calculations < describe.json | python3 -I -c 'import json,sys; print(len(json.load(sys.stdin)))')" 13

echo "== init and import"
boxscore-exact init --database "$DB" > init.json
expect "init status" "$(jget status < init.json)" created
boxscore-exact import-csv --database "$DB" \
  --actuals docs/quickstart/actuals.csv --budgets docs/quickstart/budgets.csv \
  --snapshot docs/quickstart/snapshot.json \
  --property "Sample Gardens" --period 2026-05 --units 24 > import.json
expect "import status" "$(jget status < import.json)" imported
REV="$(jget revision_id < import.json)"
expect_match "revision id is a UUID" "$REV" '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'

echo "== adapters, and the long form of the same import"
boxscore-exact adapters > adapters.json
expect "adapter profiles" "$(jget adapters < adapters.json | python3 -I -c 'import json,sys; print(",".join(a["profile"] for a in json.load(sys.stdin)))')" "canonical.csv/1,canonical.json/1"
boxscore-exact import-with --database "$DB" --profile canonical.csv/1 \
  --file actuals=docs/quickstart/actuals.csv --file budgets=docs/quickstart/budgets.csv \
  --file snapshot=docs/quickstart/snapshot.json \
  --option property="Sample Gardens" --option period=2026-05 --option units=24 > import_with.json
expect "import-with is the same import (same revision id)" "$(jget revision_id < import_with.json)" "$REV"

echo "== review"
boxscore-exact review --database "$DB" --revision "$REV" > review.json
expect "review status" "$(jget status < review.json)" calculated
expect "review_reasons" "$(jget variance.review_reasons < review.json)" "[]"
expect "actual_noi" "$(jget variance.noi_bridge.actual_noi < review.json)" 13700.00
expect "budget_noi" "$(jget variance.noi_bridge.budget_noi < review.json)" 18800.00
expect "noi_variance" "$(jget variance.noi_bridge.noi_variance < review.json)" -5100.00
expect "revenue_variance" "$(jget variance.noi_bridge.revenue_variance < review.json)" -3500.00
expect "expense_variance" "$(jget variance.noi_bridge.expense_variance < review.json)" 1600.00
expect "human_approval" "$(jget human_approval < review.json)" not_granted

echo "== cited answer"
printf '{"revision_id":"%s"}' "$REV" | boxscore-exact call --database "$DB" --function review_period > cited.json
expect "review_period status" "$(jget status < cited.json)" calculated
expect "review_period noi_variance" "$(jget result.variance.noi_bridge.noi_variance < cited.json)" -5100.00
expect "trace_store" "$(jget trace_store < cited.json)" database
CITATION="$(jget citation < cited.json)"
TRACE="$(jget trace_id < cited.json)"
expect_match "citation line" "$CITATION" "^boxscore review_period@1\.0\.0 · trace $TRACE · input sha256:[0-9a-f]{12}… · output sha256:[0-9a-f]{12}… · revision $REV\$"
echo "     $CITATION"

echo "== verify the citation"
boxscore-exact trace --database "$DB" --id "$TRACE" > trace.json
expect "trace function" "$(jget result.trace.function < trace.json)" review_period
expect "trace version" "$(jget result.trace.version < trace.json)" 1.0.0
expect "trace revision_id" "$(jget result.trace.revision_id < trace.json)" "$REV"
expect "trace output hash" "$(jget result.trace.output_sha256 < trace.json)" "$(jget output_sha256 < cited.json)"
printf '{"trace_id":"%s"}' "$TRACE" | boxscore-exact call --database "$DB" --function get_provenance > provenance.json
expect "get_provenance status" "$(jget status < provenance.json)" calculated
expect "provenance revision property" "$(jget result.revision.property < provenance.json)" "Sample Gardens"
expect "provenance source profile" "$(jget result.revision.sources.profile < provenance.json)" canonical.csv/1
printf '{"revision_id":"%s"}' "$REV" | boxscore-exact call --database "$DB" --function review_period > cited_again.json
expect "rerun gives the same output hash (review_period)" "$(jget output_sha256 < cited_again.json)" "$(jget output_sha256 < cited.json)"

echo "== snapshot metrics"
printf '{"revision_id":"%s"}' "$REV" | boxscore-exact call --database "$DB" --function occupancy > occupancy.json
expect "occupancy status" "$(jget status < occupancy.json)" review_required
expect "occupancy decimal" "$(jget result.occupancy.decimal < occupancy.json)" 0.8750
expect "occupancy review reason" "$(jget result.review_reasons < occupancy.json)" '["down_units_present"]'

echo "== what refuses"
set +e
boxscore-exact import-csv --database "$DB" \
  --actuals docs/quickstart/not_canonical.csv --budgets docs/quickstart/budgets.csv \
  --property "Sample Gardens" --period 2026-06 --units 24 > refused_layout.json
CODE=$?
set -e
expect "non-canonical CSV exit code" "$CODE" 2
expect "non-canonical CSV status" "$(jget status < refused_layout.json)" refused
expect "non-canonical CSV error" "$(jget error.code < refused_layout.json)" INVALID_INPUT
# Amount and account-code shapes the quickstart says refuse. Each is its own
# file on a fresh period so only the file under test can be the cause.
HDR='account_code,account_name,category,amount'
printf '%s\n4000,Rent,rental income,"1,200.00"\n' "$HDR" > bad_thousands.csv
printf '%s\n4000,Rent,rental income,$1200.00\n' "$HDR" > bad_currency.csv
printf '%s\n4000,Rent,rental income,(1200.00)\n' "$HDR" > bad_parens.csv
printf '%s\n4000,Rent,rental income,1200.005\n' "$HDR" > bad_places.csv
printf '%s\n 4000,Rent,rental income,1200.00\n' "$HDR" > bad_space.csv
printf '%s\n4000,Rent,rental income,1200.00\n4000,Rent,other income,5.00\n' "$HDR" > bad_conflict.csv
for pair in thousands:INVALID_INPUT currency:INVALID_INPUT parens:INVALID_INPUT places:INVALID_INPUT space:INVALID_INPUT conflict:ACCOUNT_MAPPING_CONFLICT; do
  name="${pair%%:*}"; want="${pair##*:}"
  set +e
  boxscore-exact import-csv --database "$DB" --actuals "bad_$name.csv" --budgets docs/quickstart/budgets.csv \
    --property "Sample Gardens" --period 2026-03 --units 24 > "refused_$name.json"
  CODE=$?
  set -e
  expect "bad_$name exit code" "$CODE" 2
  expect "bad_$name error" "$(jget error.code < "refused_$name.json")" "$want"
done
# Not a refusal: a row repeated in a file is summed (the quickstart warns).
printf '%s\n4000,Rent,rental income,1200.00\n4000,Rent,rental income,1200.00\n' "$HDR" > dup_rows.csv
boxscore-exact import-csv --database "$DB" --actuals dup_rows.csv --budgets docs/quickstart/budgets.csv \
  --property "Sample Gardens" --period 2026-03 --units 24 > import_dup.json
boxscore-exact review --database "$DB" --revision "$(jget revision_id < import_dup.json)" > review_dup.json
expect "repeated row is summed, not refused" "$(jget variance.by_account.0.actual < review_dup.json)" 2400.00
set +e
boxscore-exact import-csv --database "$DB" \
  --actuals docs/quickstart/actuals_corrected.csv --budgets docs/quickstart/budgets.csv \
  --snapshot docs/quickstart/snapshot.json \
  --property "Sample Gardens" --period 2026-05 --units 24 > refused_stale.json
CODE=$?
set -e
expect "second import without supersedes exit code" "$CODE" 2
expect "second import without supersedes error" "$(jget error.code < refused_stale.json)" STALE_REVISION

echo "== review_required: no snapshot"
boxscore-exact import-csv --database "$DB" \
  --actuals docs/quickstart/actuals.csv --budgets docs/quickstart/budgets.csv \
  --property "Sample Gardens" --period 2026-04 --units 24 > import_april.json
APR="$(jget revision_id < import_april.json)"
boxscore-exact review --database "$DB" --revision "$APR" > review_april.json
expect "April status" "$(jget status < review_april.json)" review_required
expect "April review reasons" "$(jget variance.review_reasons < review_april.json)" '["missing_occupancy_snapshot"]'

echo "== correction"
boxscore-exact import-csv --database "$DB" \
  --actuals docs/quickstart/actuals_corrected.csv --budgets docs/quickstart/budgets.csv \
  --snapshot docs/quickstart/snapshot.json \
  --property "Sample Gardens" --period 2026-05 --units 24 \
  --supersedes "$REV" --reason "Concessions credit of 400.00 was posted twice in the PMS export" > correction.json
expect "correction status" "$(jget status < correction.json)" imported
REV2="$(jget revision_id < correction.json)"
boxscore-exact review --database "$DB" --revision "$REV2" > review2.json
expect "corrected noi_variance" "$(jget variance.noi_bridge.noi_variance < review2.json)" -4700.00
expect "change in noi_variance" "$(jget changes.noi_variance < review2.json)" 400.00
expect "supersedes_revision" "$(jget supersedes_revision < review2.json)" "$REV"
expect "correction_reason" "$(jget correction_reason < review2.json)" "Concessions credit of 400.00 was posted twice in the PMS export"
boxscore-exact review --database "$DB" --revision "$REV" > review_old.json
expect "original revision is unchanged" "$(jget variance.noi_bridge.noi_variance < review_old.json)" -5100.00

echo "== issue"
boxscore-exact issue --database "$DB" --revision "$REV2" > issued.json
expect_match "report id" "$(jget report_id < issued.json)" '^[0-9a-f]{8}-'
expect "issued human_approval" "$(jget human_approval < issued.json)" not_granted

echo "== list revisions"
echo '{"property":"Sample Gardens","period":"2026-05"}' | boxscore-exact call --database "$DB" --function list_revisions > listed.json
expect "revisions for May" "$(jget result.revisions < listed.json | python3 -I -c 'import json,sys; print(len(json.load(sys.stdin)))')" 2

echo "== MCP over stdio (what Claude Desktop runs)"
cat > mcp_requests.jsonl <<'JSONL'
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"quickstart-check","version":"0"}}}
{"jsonrpc":"2.0","method":"notifications/initialized"}
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"list_revisions","arguments":{"property":"Sample Gardens","period":"2026-05"}}}
JSONL
boxscore-exact mcp --database "$DB" < mcp_requests.jsonl > mcp_out.jsonl
expect "MCP replies" "$(wc -l < mcp_out.jsonl | tr -d ' ')" 2
sed -n 2p mcp_out.jsonl > mcp_call.json
expect "MCP tool result is not an error" "$(jget result.isError < mcp_call.json 2>/dev/null || echo false)" false
MCP_CITATION="$(jget result.structuredContent.citation < mcp_call.json)"
expect_match "MCP citation line" "$MCP_CITATION" '^boxscore list_revisions@1\.0\.0 · trace '

echo
echo "quickstart check passed: $PASS assertions"
