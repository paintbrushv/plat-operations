# Glossary

Terms as this repository uses them, in the order a new reader tends to meet
them. Each entry points at the document that is authoritative. Examples use the
synthetic "Sample Gardens" data in [`quickstart/`](quickstart/); see the
[QUICKSTART](../QUICKSTART.md).

## Operating numbers

**NOI (net operating income).** Revenue minus operating expenses for a period,
before debt service and capital spending. In the engine, revenue is rental
income plus other income minus concessions and bad debt (the last two are
stored as negatives), and expenses are the positive-cost lines. Lines
categorised `unmapped` are shown separately and are not part of NOI. Sample
Gardens, May 2026: actual NOI 13,700.00, budget NOI 18,800.00.

**NOI bridge.** The set of totals that explain actual versus budget NOI:
`actual_revenue`, `budget_revenue`, `revenue_variance`, `actual_expenses`,
`budget_expenses`, `expense_variance`, `actual_noi`, `budget_noi`,
`noi_variance`, plus `unmapped_actual` and `unmapped_budget`. Every variance is
actual minus budget, so a negative `noi_variance` (-5100.00) is a miss. In
`review_period` the bridge covers only accounts present on both the actual and
budget side (`bridge_scope: matched_accounts_only`); the others are listed in
`excluded_accounts`. See [EXACT_CENTS.md](EXACT_CENTS.md).

**GL (general ledger).** The accounting ledger a property's income and expenses
are posted to. The engine's actuals and budgets are GL rows: an account code,
an account name, a category and an amount.

**PMS (property management system).** The operating system of record a
property manager runs; its exports are what you map into the canonical layout.
No PMS connector ships.

**Contra-revenue.** A line that reduces revenue rather than adding to it, such
as concessions or bad debt. Supplied as a **negative** amount in the revenue
section (`-1200.00`). The engine adds it, so it lowers revenue.

**Positive-costs convention.** Expenses are supplied as **positive** numbers
(a cost of 8,000.00 is `8000.00`); a credit to an expense is negative. Every
dataset states `expense_convention: positive_costs`, and anything else is
refused. If an expense total comes out below zero the review raises
`negative_net_expenses`, because that usually means the source used the
opposite sign.

**Ratio.** A rate expressed without floating point:
`{"numerator":"21","denominator":"24","decimal":"0.8750","scale":4}`. The
numerator and denominator are the integers it was computed from (cents, for
money ratios), `decimal` is rounded half away from zero to `scale` (always 4)
places using integer arithmetic, and all values but `scale` are strings. A zero
denominator refuses with `ZERO_DENOMINATOR`. Occupancy, economic occupancy and
delinquency return ratios.

## Data and history

**Canonical dataset.** One property-period in the shape the engine stores:
property name, period (`YYYY-MM`), currency (`USD`), expense convention, unit
count, actual rows, budget rows (each `account_code`, `account_name`,
`category`, `amount`) and an optional snapshot of unit counts and rent roll
totals. Everything the engine calculates starts from one. Its SHA-256 is the
revision's `input_sha256`. Vendor exports must be mapped into it before
import; see the [QUICKSTART](../QUICKSTART.md#load-your-own-export).

**Adapter profile.** A versioned id such as `canonical.csv/1` that selects the
source adapter used to turn named files into a canonical dataset. A profile's
spec says which files and options it takes and what makes it refuse;
`boxscore-exact adapters` prints them. Changing what a profile accepts means a
new profile version (`canonical.csv/2`), never a silent edit. The profile used
is recorded in the revision's `sources`. See
[DATA_CONTRACTS.md](../boxscore/docs/DATA_CONTRACTS.md#adapters-exact-cent-ingestion).

**Revision.** One immutable stored version of a property-period's canonical
dataset, with an id, a revision number, a parent (if it corrects another), the
SHA-256 of each source file, and a creation time. A revision can never be
edited or deleted; storage triggers refuse. Importing identical content again
returns the same revision.

**Correction / supersedes.** A correction is a new revision of a period that
already has one. It must name the current revision (`supersedes`, the
`--supersedes` flag) and give a `reason`; otherwise the import refuses with
`STALE_REVISION`. The parent chain is the audit trail, and reviewing the new
revision reports the change in every bridge value against its parent. See the
[QUICKSTART](../QUICKSTART.md#correct-a-period).

**Issued report.** An immutable record created by `issue_report` (or the
`issue` command) for a revision. It stores the full review body at that moment,
with a new `report_id` and `issued_at`; issuing the same revision again makes a
new report with a different hash and never changes the old one. Only the latest
revision of a property and period can be issued: a revision a correction has
superseded refuses with `SUPERSEDED`, naming the latest. It always says
`human_approval: not_granted`: issuing is a record, not sign-off.

**Review reason.** A named condition that makes a result `review_required`
instead of `calculated`: the arithmetic completed, and a person should look
before relying on it. See [Review reasons](#review-reasons).

## Calls and citations

**Envelope.** The JSON wrapper every catalog function returns, refusals
included: `contract_version` (`plat.ops/1`), `function`, `version`, `producer`,
`input_sha256`, `output_sha256`, `computed_at`, `trace_id`, `trace_store`,
`citation`, `status` (`calculated`, `review_required` or `refused`), and either
`result` or a typed `error`. See [EXACT_CENTS.md](EXACT_CENTS.md#calculation-catalog-and-versioning-policy).

**Trace id.** A UUID v4 assigned to every call into the catalog. With a host
database, the call is written to the append-only `exact_invocations` table, and
`boxscore-exact trace --id ID` (or the `get_provenance` function) reads the
record back. `trace_store` says where the record went: `database`, `jsonl` or
`none`. See [PROVENANCE.md](PROVENANCE.md).

**Citation.** The one-line summary in the envelope, meant to be pasted next to
a number: `boxscore review_period@1.0.0 · trace <id> · input sha256:<12 hex> ·
output sha256:<12 hex> · revision <id>`. It names the function and exact
version, the trace id, the first 12 hex characters of both hashes (or
`refused CODE`), and any revision or report the call touched. It shows what the
engine computed from what it was given, not that the inputs were right.

**`input_sha256` / `output_sha256`.** SHA-256 (Secure Hash Algorithm, 256-bit)
fingerprints. `input_sha256` hashes the input bytes exactly as the transport
received them (canonical JSON for in-process calls). `output_sha256` hashes the
canonical JSON of `result`. The same input to the same `function@version`
always gives the same `output_sha256`, so a number can be reproduced from its
citation. Stored revisions use the same idea: a review re-checks stored rows
against the revision's `input_sha256` and refuses with `INTEGRITY_ERROR` if
they changed.

**MCP (Model Context Protocol).** The open protocol an assistant such as Claude
uses to call tools. `boxscore-exact mcp` (stdio) and `boxscore-exact serve`
(HTTP, at `/mcp`) expose each catalog function as one tool. The server's
grounding rule tells the model to take numbers only from the engine and to cite
them. See [connectors/claude-desktop.md](connectors/claude-desktop.md).

**Stateless vs stateful function.** A stateless function (`describe`,
`noi_variance`, and the three snapshot metrics given an inline snapshot) needs
no database. A stateful function (`import_period`, `review_period`,
`issue_report`, `list_revisions`, `close_readiness`, `t12`, `get_provenance`;
the snapshot metrics when given a `revision_id`) reads or writes the database
the host process was started with, and refuses with `DATABASE_REQUIRED` without
one. The database path comes only from the host (`--database`), never from the
input.

**Loopback.** The local-only network address of your own machine
(`127.0.0.1`). `boxscore-exact serve` binds to loopback by default, so only
programs on the same machine can reach it. It refuses to bind any other address
unless an API key file is given. Nothing listens at all unless you start
`serve`; the stdio `mcp` mode uses no network. See
[connectors/remote-mcp.md](connectors/remote-mcp.md).

## Review reasons

Every reason the code emits, in `review_reasons` (or
`blocking_review_reasons` for `close_readiness`). Source: `boxscore/src/exact/`.

| reason | raised by | meaning |
|---|---|---|
| `missing_actuals` | `review_period` (and `issue_report`) | the revision has no actual rows |
| `missing_budgets` | `review_period` | the revision has no budget rows |
| `missing_occupancy_snapshot` | `review_period` | no snapshot was imported with the revision |
| `incomplete_account_coverage` | `review_period` | some account code has an actual but no budget row, or the reverse; those rows are excluded from the bridge and listed in `excluded_accounts` |
| `unmapped_accounts` | `review_period`, `t12` | at least one line has category `unmapped`; in `t12` it is raised when a present month's unmapped total is nonzero |
| `negative_net_expenses` | `review_period`, `noi_variance` | total actual or budget expenses are below zero, which usually means a sign problem |
| `missing_periods` | `t12` | a month in the 12-month window has no stored revision; it is shown as null and never filled with zero |
| `down_units_present` | `occupancy` | some units are down (offline); they stay in the denominator. Informational |
| `in_place_exceeds_market` | `economic_occupancy` | in-place rent is above market rent, which suggests a rent roll or market rent problem |
| `missing_prepaid` | `delinquency` | the snapshot has no prepaid amount, so `net` and the prepaid share are null |
| `missing_in_place_rent` | `delinquency` | in-place rent is missing or zero, so the shares are null |
| `no_revisions_for_period` | `close_readiness` | no property has a revision for the requested period |
| `properties_not_ready` | `close_readiness` | at least one property is `not_ready` (missing actuals, budgets or an in-period snapshot, a blocking reason from `review_period`, or no issued report) |

`close_readiness` also reports a per-property `snapshot` value of `missing`,
`present` or `outside_period`; those are fields, not review reasons.

## Where each term is defined in code

| term | code |
|---|---|
| envelope, versions, the 12 functions | `boxscore/src/exact/catalog.rs` |
| revisions, corrections, issued reports | `boxscore/src/exact/store.rs` |
| NOI bridge, positive costs | `boxscore/src/exact/variance.rs` |
| ratios | `boxscore/src/exact/ratio.rs` |
| adapters, profiles | `boxscore/src/exact/ingest.rs` |
| trace ids, citations | `boxscore/src/exact/trace.rs` |
