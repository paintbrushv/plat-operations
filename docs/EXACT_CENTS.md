# Exact-cent operating workflow

`boxscore-exact` is the v0.1 candidate operating producer. It uses checked
signed integer USD cents for GL imports, snapshot money, variance, corrections,
issued totals, and SQLite persistence. Rates and occupancy counts are separate.
The general `boxscore` commands still use legacy floating-point money and are
excluded from this exact-cent workflow.

## Build and run

```bash
cd boxscore
cargo build --locked --release --bin boxscore-exact
export PATH="$PWD/target/release:$PATH"
boxscore-exact init --database ./exact.sqlite
boxscore-exact import-csv --database ./exact.sqlite \
  --actuals actuals.csv --budgets budgets.csv \
  --property synthetic_ops --period 2026-05 --units 10
boxscore-exact review --database ./exact.sqlite --revision REVISION_ID
boxscore-exact issue --database ./exact.sqlite --revision REVISION_ID
```

Canonical CSV headers: `account_code,account_name,category,amount`. Monetary
cells are decimal text, for example `100.10`, `-5.01`, or `0.00`. Vendor exports
must first be mapped to this schema; this command does not infer their layout,
account approval, signs, or currency. Classification uses the existing Rust
ontology: rental income, concessions, bad debt, and other income are revenue;
`unmapped` is disclosed separately; other supplied categories are expenses.
Supply expenses as positive costs, credits as negatives, and contra-revenue as
negatives. The producer preserves signs and flags negative net expenses.

`import --input period.json` accepts the same canonical data:

```json
{
  "property": "synthetic_ops", "period": "2026-05",
  "currency": "USD", "expense_convention": "positive_costs", "unit_count": 10,
  "actuals": [{"account_code":"4000","account_name":"Rent","category":"rental income","amount":"100.10"}],
  "budgets": [{"account_code":"4000","account_name":"Rent","category":"rental income","amount":"90.05"}],
  "snapshot": {
    "as_of_date":"2026-05-31", "occupied_units":9,"vacant_units":1,"down_units":0,
    "market_rent_total":"110.00", "in_place_rent_total":"100.10",
    "delinquent_amount":null,"prepaid_amount":null,"concessions_amount":null
  }
}
```

The optional `--snapshot snapshot.json` CSV argument accepts the snapshot object
above. Occupied, vacant, and down units must sum to the stated unit count.
Missing snapshots and missing GL sides require review. Review totals cover only
account codes with both actual and budget rows; excluded rows retain their
amounts and reasons. A stated zero budget is included. Each CSV row contributes
once; repeated full imports with identical canonical content are idempotent.
Reconcile duplicate rows in the source before importing.

Corrections require `--supersedes CURRENT_REVISION_ID --reason "..."` on import.
Every correction adds a revision. Reports include the change in each bridge
value versus the previous revision. Issuance always creates a new report ID;
old report bodies stay unchanged. `calculated` means arithmetic completed.
Every report says `human_approval: not_granted`.

## Money and persistence contract

- JSON money must be a string with at most two decimal places. Output always
  has two. Floats, exponent notation, NaN, and excess precision refuse.
- Supported magnitude: at most `92233720368547758.07` USD, positive or negative.
  Every addition and subtraction is checked, including aggregate totals.
- Canonical input files and protocol requests are limited to 2 MiB; the
  calculation also caps the combined row count at 100,000.
- Database `application_id` is `0x504c4154`, schema version is 1. Money columns
  are INTEGER cents with type checks. Imports commit atomically. Revisions,
  snapshots, and reports have UPDATE/DELETE refusal triggers.
- Input and issued-body SHA-256 values identify their content. Reviews verify
  stored canonical input against its hash. These are integrity checks, not a
  substitute for operating-system access controls.
- Exact and legacy databases are distinct. New and old consumers reject the
  wrong schema instead of interpreting cents as dollars.

## Calculation catalog and versioning policy

Every calculation the agent connectors can reach is a named, versioned
function in `boxscore::exact::catalog`. Run one directly:

```bash
echo '{}' | boxscore-exact call --function describe
echo '{"revision_id":"REVISION_ID"}' | boxscore-exact call --database ./exact.sqlite --function review_period
```

| function | stateful | what it returns |
|---|---|---|
| `describe@1.0.0` | no | the catalog with JSON Schemas for every function |
| `noi_variance@1.0.0` | no | the `plat.ops/1` variance bridge from inline rows |
| `import_period@1.0.0` | yes | `revision_id` for a canonical dataset (`supersedes` + `reason` for corrections) |
| `review_period@1.0.0` | yes | the same body as `review` |
| `issue_report@1.0.0` | yes | the same body as `issue` |
| `list_revisions@1.0.0` | yes | stored revisions and their issued reports |
| `occupancy@1.0.0` | no | occupied units over unit count as a ratio, with the unit counts and `down_units_present` review reason |
| `economic_occupancy@1.0.0` | no | in-place rent over market rent as a ratio, both totals echoed; refuses `MISSING_MARKET_RENT`, `MISSING_IN_PLACE_RENT`, `ZERO_DENOMINATOR` |
| `delinquency@1.0.0` | no | delinquent, prepaid and net balances, each as a share of in-place rent; refuses `MISSING_DELINQUENCY` |
| `t12@1.0.0` | yes | trailing twelve-period statement for `property` ending at `end_period`: category rows, NOI, budgets and actual-minus-budget NOI per month, the revision used for each month, and `missing_periods`; refuses `NOT_FOUND` when the property has no revision in the window |

The three snapshot metrics (`occupancy`, `economic_occupancy`, `delinquency`) take exactly one of `{"revision_id": "..."}` or
`{"snapshot": {...}, "unit_count": N}`. They are not stateful: with an inline
snapshot they never open a database. With `revision_id` the host must supply
`--database` (otherwise `DATABASE_REQUIRED`) and the stored revision must have
a snapshot (otherwise `MISSING_OCCUPANCY_SNAPSHOT`). An inline snapshot is
validated like an imported one: canonical date and non-negative counts that sum
to `unit_count`. Any review reason makes the status `review_required`.

`t12` takes `{"property", "end_period": "YYYY-MM", "revision_policy"}`. Each of
the twelve months uses one stored revision, checked against its input hash:
`latest` (default) is the highest revision number, `issued_only` the highest
revision with an issued report. A month with no revision is `null` in every row
and in `noi`, is listed in `missing_periods`, and makes the status
`review_required` (`missing_periods`); totals cover only the present months,
and nothing is ever filled with zero. Categories are folded to their reviewed
spelling (`rental income` and `Rental Income` share a row), ordered revenue,
expenses, then other categories alphabetically; rows with no activity are
omitted. NOI is revenue minus expenses; `unmapped` rows are shown but excluded
from NOI and totalled separately. `budgets` is present only when every month
that has a revision also has budget rows, and `variance` (actual minus budget
NOI) only alongside it. Unlike `review_period`, which bridges only accounts
present in both actuals and budgets, `t12` reports every stored line.

Every response is an envelope:

```json
{"contract_version":"plat.ops/1","function":"noi_variance","version":"1.0.0",
 "producer":{"name":"boxscore-exact","version":"0.1.1","arithmetic":"checked_i64_cents"},
 "input_sha256":"…","output_sha256":"…","computed_at":"2026-10-11T15:00:00Z",
 "status":"calculated","result":{…}}
```

`input_sha256` is the SHA-256 of the input bytes exactly as the transport
received them (for in-process calls, of the canonical JSON serialization).
`output_sha256` is the SHA-256 of the canonical JSON serialization of
`result`; the same input to the same `function@version` always produces the
same `output_sha256`, so a number can be reproduced from its citation.
Refusals keep the envelope with `status: "refused"` and a typed `error`;
`boxscore-exact call` exits 2 on a refusal and 0 otherwise.

Versions are per function and independent of the contract version. A
**patch** bump changes nothing observable in `result`; a **minor** bump only
adds fields; a **major** bump may change a number for the same input. Pass
`--version` (or `"version"` on the protocol) to pin one; a different served
version refuses with `VERSION_MISMATCH` instead of silently answering.

Stateful functions take `--database` from the host process only. The input
JSON has no path, SQL or network field, and the stateless `protocol` refuses
them with `DATABASE_REQUIRED`. Legacy floating-point calculations are never
part of the catalog.

### Ratios

Rates are never floats. A ratio is
`{"numerator":"153","denominator":"172","decimal":"0.8895","scale":4}`: the
integer numerator and denominator it was computed from (cents, for money
ratios), and a decimal rounded half away from zero to a fixed scale of 4 in
integer arithmetic. All values are JSON strings except `scale`; a negative ratio
carries a leading minus (`-0.3333`), and a value that rounds to zero is
`0.0000`. A zero denominator refuses with `ZERO_DENOMINATOR`.

## Rust stdin/stdout protocol

Run exactly `boxscore-exact protocol`, write one JSON request, then close stdin:

```json
{"contract_version":"plat.ops/1","operation":"variance","currency":"USD","expense_convention":"positive_costs","actuals":[],"budgets":[]}
```

Any other `operation` names a catalog function (for example `describe` or
`noi_variance`); the remaining fields are its input and the response is the
provenance envelope described above. The `variance` operation keeps its
original response shape.

This low-level function computes the supplied rows, including stated one-sided
rows. The period-review layer applies missing-account coverage gates first.
Responses contain `contract_version`, `status`, `input_sha256`, producer name,
version and `arithmetic: checked_i64_cents`, and a result with `by_account`,
`noi_bridge`, and `review_reasons`. Refusals contain a typed `error` and exit 2;
success exits 0. Unknown fields and operations refuse. Protocol requests cannot
select paths, executables, SQL, or network resources.

The `platworks.ops_oracle` client invokes these fixed arguments with a 60-second
timeout. It verifies the contract, input hash, producer identity, and decimal
response shape. It records the executable SHA-256. Set
`PLAT_BOXSCORE_EXACT_BIN` on the host and optionally pin
`PLAT_BOXSCORE_EXACT_SHA256`. No Python financial fallback is used.

## Reviewed migration of a legacy database

Close the legacy writer and checkpoint it before starting. Nonempty WAL or
journal files refuse migration. Planning and migration use read-only source
connections. Never run this over an actively written database.

```bash
boxscore-exact plan-migration --source legacy.sqlite > migration-plan.json
# A person reviews the plan and prepares review.json, bound to its source hash.
boxscore-exact migrate --source legacy.sqlite --destination reviewed-copy --review review.json
```

Review schema:

```json
{"contract_version":"plat.ops-migration-review/1","source_sha256":"HASH_FROM_PLAN","reviewer":"REVIEWER","reviewed_at":"2026-10-03","rounding":"reject","acknowledge_negative_expenses":false,"acknowledge_snapshot_selection":false,"expense_convention":"positive_costs"}
```

Use `rounding: half_away_from_zero` only after reviewing the listed rounding
changes. Negative expense signs are preserved; acknowledgement records review,
not a sign flip. Multiple snapshots require explicit selection acknowledgement.
The rule selects the latest snapshot within each GL period, then created time,
then ID. Property IDs are preserved to avoid merging same-name properties.
Orphaned GL rows refuse migration. All table counts are recorded.

The new directory contains the byte-identical, read-only `legacy.sqlite`, a new
`exact.sqlite`, and `migration-report.json`. Historical issued Markdown is also
copied verbatim into `archived_reports`. Original source hashes are checked
before and after. Subcent transformations, sign exceptions, and snapshot
selections remain recorded with review metadata. Stale reviews refuse.
Incomplete migrations remain for inspection with an unsupported schema version.

Migration covers GL periods and associated rent-roll, delinquency, prepaid,
and concession snapshot money. Other legacy tables remain preserved in the
archive. Transactions, unit leases/receivables, collection analytics, turn costs,
unit P&L, forecasts, legacy T12, dashboards, and legacy vendor import commands
are outside the exact-cent release workflow.

## Validation and release limits

Tests execute the actual binary, verify integer SQLite storage, checked overflow,
credits/reversals, immutable issued reports, correction bridges, missing-budget
exclusions, and reviewed source-preserving migration. Python runs the producer
and a CSV → persistence → correction → harness review acceptance test.

Binary distribution and clean platform installs remain release work. The root
repository license and existing Cargo license metadata disagree; settle that
before publishing a Rust package. No real customer database has been migrated
as part of these synthetic tests.

Account-code matching tolerates different display names and category case or
surrounding whitespace. Account codes themselves must not carry leading or
trailing whitespace; such rows refuse with `INVALID_INPUT` rather than
splitting one account into two. The producer uses a deterministic display name and a
normalized category. Conflicting categories for one code refuse with
`ACCOUNT_MAPPING_CONFLICT`; resolve the mapping before import or calculation.
