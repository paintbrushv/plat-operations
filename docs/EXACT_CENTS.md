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
boxscore-exact import-with --database ./exact.sqlite --profile canonical.csv/1 \
  --file actuals=actuals.csv --file budgets=budgets.csv \
  --option property=synthetic_ops --option period=2026-05 --option units=10
boxscore-exact review --database ./exact.sqlite --revision REVISION_ID
boxscore-exact issue --database ./exact.sqlite --revision REVISION_ID
```

## Source adapters

Every import goes through a source adapter selected by a versioned profile id
(`name/N`). `boxscore-exact adapters` prints each registered adapter's spec:
its files (with expected header or shape), options, and what makes it refuse.

```bash
boxscore-exact adapters
boxscore-exact import-with --database ./exact.sqlite --profile canonical.json/1 --file input=period.json
```

`import-with` takes `--profile`, one `--file NAME=PATH` per input file, one
`--option KEY=VALUE` per option, and `--supersedes`/`--reason` for
corrections. A file or option the spec does not declare refuses with
`UNKNOWN_FILE`/`UNKNOWN_OPTION`; an absent required one with
`MISSING_FILE`/`MISSING_OPTION`; an unknown profile with `UNKNOWN_PROFILE`,
listing the registered ones. Each file is capped at 2 MiB. The revision's
`sources` records `profile` alongside the SHA-256 of each file read.

| profile | files | options |
|---|---|---|
| `canonical.csv/1` | `actuals`, `budgets`, optional `snapshot` | `property`, `period`, `units` |
| `canonical.json/1` | `input` | none |

The older subcommands are aliases with unchanged arguments, refusals, and
output; the only difference is that the stored `sources` now also names the
`profile`:
`import-csv --actuals A --budgets B [--snapshot S] --property P --period YYYY-MM --units N`
is `import-with --profile canonical.csv/1`, and `import --input F` is
`import-with --profile canonical.json/1`. The stored `input_sha256` is the
hash of the canonical dataset, identical through either front end. How to
write and version a new adapter is in `boxscore/docs/DATA_CONTRACTS.md`.

## Canonical data

Canonical CSV headers: `account_code,account_name,category,amount`. Monetary
cells are decimal text, for example `100.10`, `-5.01`, or `0.00`. Vendor exports
must first be mapped to this schema; this command does not infer their layout,
account approval, signs, or currency. Classification uses the existing Rust
ontology: rental income, concessions, bad debt, and other income are revenue;
`unmapped` is disclosed separately; other supplied categories are expenses.
Supply expenses as positive costs, credits as negatives, and contra-revenue as
negatives. The producer preserves signs and flags negative net expenses.

`canonical.json/1` (`import --input period.json`) accepts the same canonical data:

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

The optional `snapshot` file of `canonical.csv/1` (`--snapshot snapshot.json`) accepts the snapshot object
above. Occupied, vacant, and down units must sum to the stated unit count.
Missing snapshots and missing GL sides require review. Review totals cover only
account codes with both actual and budget rows; excluded rows retain their
amounts and reasons. A stated zero budget is included. Each CSV row contributes
once; repeated full imports with identical canonical content are idempotent.
Reconcile duplicate rows in the source before importing.

Corrections require `--supersedes CURRENT_REVISION_ID --reason "..."` on any import subcommand.
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
- Database `application_id` is `0x504c4154`, schema version is 2 (2 added the
  `exact_invocations` trace table; a schema-1 database refuses with
  `SCHEMA_MISMATCH` and must be re-imported or rebuilt by reviewed copy
  migration). Money columns are INTEGER cents with type checks. Imports commit
  atomically. Revisions, snapshots, reports, and invocation records have
  UPDATE/DELETE refusal triggers.
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
| `describe@1.1.0` | no | the catalog with JSON Schemas for every function, plus `adapters` (the source adapter specs) |
| `noi_variance@1.0.0` | no | the `plat.ops/1` variance bridge from inline rows |
| `import_period@1.0.0` | yes | `revision_id` for a canonical dataset (`supersedes` + `reason` for corrections) |
| `review_period@1.0.0` | yes | the same body as `review` |
| `issue_report@1.0.0` | yes | the same body as `issue` |
| `list_revisions@1.0.0` | yes | stored revisions and their issued reports |
| `occupancy@1.0.0` | no | occupied units over unit count as a ratio, with the unit counts and `down_units_present` review reason |
| `economic_occupancy@1.0.0` | no | in-place rent over market rent as a ratio, both totals echoed; refuses `MISSING_MARKET_RENT`, `MISSING_IN_PLACE_RENT`, `ZERO_DENOMINATOR` |
| `delinquency@1.0.0` | no | delinquent, prepaid and net balances, each as a share of in-place rent; refuses `MISSING_DELINQUENCY` |
| `close_readiness@1.0.0` | yes | per property with a revision for `period` (optionally one `property`): the latest revision, `ready` or `not_ready`, account coverage counts, blocking review reasons from `review_period`, issued reports, and `not_tracked_in_exact_store` |
| `t12@1.0.0` | yes | trailing twelve-period statement for `property` ending at `end_period`: category rows, NOI, budgets and actual-minus-budget NOI per month, the revision used for each month, and `missing_periods`; refuses `NOT_FOUND` when the property has no revision in the window |
| `get_provenance@1.0.0` | yes | the recorded invocation for a `trace_id`, with the revision (`sources`, `reason`, parent) and report hash it names; unknown ids refuse `NOT_FOUND` |

The three snapshot metrics (`occupancy`, `economic_occupancy`, `delinquency`) take exactly one of `{"revision_id": "..."}` or
`{"snapshot": {...}, "unit_count": N}`. They are not stateful: with an inline
snapshot they never open a database. With `revision_id` the host must supply
`--database` (otherwise `DATABASE_REQUIRED`) and the stored revision must have
a snapshot (otherwise `MISSING_OCCUPANCY_SNAPSHOT`). An inline snapshot is
validated like an imported one: canonical date and non-negative counts that sum
to `unit_count`. Any review reason makes the status `review_required`.

`close_readiness` takes `{"period": "YYYY-MM", "property": optional}` and needs
the host's `--database`. For each property it uses the latest revision of the
period and reports `ready` only when actuals, budgets and an in-period snapshot
are present, `store::review` raises no review reason, and at least one report
has been issued for that revision. Coverage counts are distinct account codes.
Collections context and the weekly BDDRE/RPCOE inputs the legacy check covers
are not in the exact store and are listed under `not_tracked_in_exact_store`.
The envelope is `review_required` when any property is `not_ready` or none
matched (`no_revisions_for_period`), otherwise `calculated`. Tampered rows
refuse `INTEGRITY_ERROR`.

`t12` takes `{"property", "end_period": "YYYY-MM", "revision_policy"}`. Each of
the twelve months uses one stored revision, checked against its input hash:
`latest` (default) is the highest revision number, `issued_only` the highest
revision with an issued report. A month with no revision is `null` in every row
and in `noi`, is listed in `missing_periods`, and makes the status
`review_required` (`missing_periods`); totals cover only the present months,
and nothing is ever filled with zero. Categories are folded to their reviewed
spelling (`rental income` and `Rental Income` share a row), ordered revenue,
expenses, then other categories alphabetically; rows with no activity are
omitted. NOI is revenue minus expenses; `Unmapped` rows are shown but excluded
from NOI and totalled separately, and any nonzero unmapped total in a present
month adds the review reason `unmapped_accounts` (as in `review_period`).
`budgets` is present only when every month
that has a revision also has budget rows, and `variance` (actual minus budget
NOI) only alongside it. Unlike `review_period`, which bridges only accounts
present in both actuals and budgets, `t12` reports every stored line.

Every response is an envelope:

```json
{"contract_version":"plat.ops/1","function":"noi_variance","version":"1.0.0",
 "producer":{"name":"boxscore-exact","version":"0.1.1","arithmetic":"checked_i64_cents"},
 "input_sha256":"…","output_sha256":"…","computed_at":"2026-10-11T15:00:00Z",
 "trace_id":"3f0c9b52-…","trace_store":"database",
 "citation":"boxscore noi_variance@1.0.0 · trace 3f0c9b52-… · input sha256:1b7638a0c2d4… · output sha256:ce5487f1e2a3…",
 "status":"calculated","result":{…}}
```

`input_sha256` is the SHA-256 of the input bytes exactly as the transport
received them (for in-process calls, of the canonical JSON serialization).
`output_sha256` is the SHA-256 of the canonical JSON serialization of
`result`; the same input to the same `function@version` always produces the
same `output_sha256`, so a number can be reproduced from its citation.
Refusals keep the envelope with `status: "refused"` and a typed `error`;
`boxscore-exact call` exits 2 on a refusal and 0 otherwise.

Every envelope, refusals included, also carries a `trace_id` (UUID v4) and a
`citation`: one line naming `function@version`, the trace id and the first 12
hex characters of the input and output hashes (or `refused CODE`), plus the
revision and report it touched. Paste it into a memo. With a host database the
call is recorded in the append-only `exact_invocations` table and
`boxscore-exact trace --database DB --id TRACE_ID` (or `get_provenance`) reads
it back; `trace_store` says where the record went. See
[PROVENANCE.md](PROVENANCE.md) for verification, its limits, and the
`BOXSCORE_TRACE_INPUTS` and `BOXSCORE_TRACE_DIR` controls.

Versions are per function and independent of the contract version. A
**patch** bump changes nothing observable in `result`; a **minor** bump only
adds fields; a **major** bump may change a number for the same input. Pass
`--version` (or `"version"` on the protocol) to pin one; a different served
version refuses with `VERSION_MISMATCH` instead of silently answering.

`describe` lists the source adapters for the operator's benefit only; the
catalog never reads files, and `import_period` takes a dataset, not paths.
Stateful functions take `--database` from the host process only. The input
JSON has no path, SQL or network field, and the stateless `protocol` refuses
them with `DATABASE_REQUIRED`. Legacy floating-point calculations are never
part of the catalog.

`boxscore-exact mcp [--database PATH]` serves the same catalog as an MCP
server over stdio, one tool per function, for Claude Desktop and other MCP
clients; see [connectors/claude-desktop.md](connectors/claude-desktop.md).
`boxscore-exact serve` serves the catalog over HTTP (MCP Streamable HTTP at
`/mcp`, `POST /v1/calc/{function}`, and the contract at `/openapi.json`), with
bearer API keys; see [connectors/remote-mcp.md](connectors/remote-mcp.md).

The three connectors are held to one contract by a golden corpus in
[`boxscore/tests/fixtures/catalog/`](../boxscore/tests/fixtures/catalog/README.md).
`cargo test --test exact_conformance` runs every case through `catalog::invoke`,
the MCP server and the HTTP router (`/v1/calc` and `/mcp`) and requires the same
`result`, `status`, `error.code`, `output_sha256`, `function` and `version` from
all of them, a match with the pinned `expected.json`, and a `result` that
satisfies the function's output schema. Per-invocation fields (`trace_id`,
`citation`, `computed_at`, `trace_store`, `trace_warning`) differ on every call
and are checked for shape only. Each `expected.json` pins the function
`version`, so a changed number under an unchanged version fails. After bumping a
version, regenerate the affected goldens with
`BOXSCORE_UPDATE_GOLDENS=1 cargo test --test exact_conformance` and review the
diff; the variable only writes goldens that are missing or pin an older
version. Adding a case is dropping `<case>.input.json` and `<case>.expected.json`
into the function's directory, and every catalog function must have one.

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
