# Data Contracts

All v0.1 ingestion uses CSV files with headers.

## Properties

`name,market,unit_count,owner_entity,property_manager`

## GL Actuals And Budgets

`property,period,account_code,account_name,category,amount`

- `property` must match an ingested property name.
- `period` must be `YYYY-MM`. Any other spelling refuses the file.
- `category` must be one of the reviewed NOI categories in `ontology::NOI_CATEGORIES`
  (`Rental Income`, `Concessions`, `Bad Debt`, `Other Income`, `Payroll`,
  `Repairs & Maintenance`, `Utilities`, `Marketing`, `Administrative`, `Taxes`,
  `Insurance`, `Management Fees`) or `Unmapped`, matched case-insensitively and
  stored in canonical spelling. Any other value refuses the file: an unknown
  category would otherwise be summed into expenses silently.
- `amount` is numeric. `$`, thousands separators and `%` are stripped;
  `(500.00)` means `-500.00`; blank, `-` and `()` mean zero; `nan`, `inf`
  and unbalanced parentheses refuse the row.
- Sign convention (see `ISSUED_REPORTS_AND_SIGN_CONVENTION.md`): revenue is
  positive, contra-revenue (`Concessions`, `Bad Debt`) is negative inside the
  revenue total, and expenses carry their natural **positive** sign, so
  `NOI = revenue - expenses`. A negative net expense total blocks report issuance.
- A file whose header does not carry every contract column, or whose rows are
  ragged, refuses before any row is written. Nothing is deleted or inserted for
  a file that fails validation.

## Rent Roll Snapshots

`property,as_of_date,occupied_units,vacant_units,leased_units,notice_units,down_units,market_rent_total,in_place_rent_total`

## Delinquency Snapshots

`property,as_of_date,delinquent_amount,delinquent_units,prepaid_amount`

## Leasing Snapshots

`property,as_of_date,leads,tours,applications,approvals,move_ins,move_outs,concessions_amount`

## Account Mapping Review

`source_system,property_scope,account_code,account_name,current_category,suggested_category,confidence_score,status,reviewed_category,review_notes`

- Exported by `boxscore accounts export-review --file <path>`.
- Imported by `boxscore accounts import-review --file <path>`.
- Only `status=approved` rows are imported.
- Approved rows must include `reviewed_category`.
- `review_notes` should explain the operator rationale when the mapping is not obvious.

## Lineage

Every ingested row stores `source_file` and `source_row` where applicable.

## Adapters (exact-cent ingestion)

The exact-cent core (`boxscore::exact`) never assumes a vendor layout. Every
file shape it can import is a **source adapter**: an implementation of
`exact::ingest::SourceAdapter` that turns named local files and string
options into the vendor-neutral `store::Dataset` plus a `sources` provenance
object recorded verbatim on the revision. Hosts select an adapter by a
versioned **profile id** and run it with `boxscore-exact import-with`;
`boxscore-exact adapters` (and the catalog's `describe`) lists every
registered adapter's spec.

### Shipped profiles

| profile | files | options | `sources` recorded |
|---|---|---|---|
| `canonical.csv/1` | `actuals`, `budgets` (header `account_code,account_name,category,amount`); optional `snapshot` (JSON object) | `property`, `period` (`YYYY-MM`), `units` (whole number) | `profile`, `actuals_sha256`, `budgets_sha256`, `snapshot_sha256` (null when absent) |
| `canonical.json/1` | `input` (canonical period dataset JSON) | none | `profile`, `canonical_json_sha256` |

`import-csv` and `import` are aliases for these two profiles. The revision's
`input_sha256` is the SHA-256 of the canonical `Dataset`, so the same content
hashes the same through any front end.

### Profile ids are versioned and fail closed

- A profile id is `name/N`: a lowercase name of `[a-z0-9._-]` and a positive
  integer version, for example `canonical.csv/1` or `yardi.gl/1`.
- Ids match exactly. An unknown id refuses with `UNKNOWN_PROFILE` and lists the
  registered ids; there is no prefix match, default, or "latest".
- Any change to what an adapter accepts or how it maps a file into a
  `Dataset` is a new id (`.../2`) registered alongside the old one, never an
  edit to an existing id. A stored revision's `sources.profile` therefore
  always names the mapping that produced it.

### Writing an adapter

1. Implement `SourceAdapter` in `boxscore/src/exact/` (or a submodule):
   `profile()` returns the id, `spec()` returns an `AdapterSpec` naming every
   file (with its expected header or shape), every option, and what makes it
   refuse; `read()` returns `(Dataset, sources)`.
2. Read files only through `AdapterInputs::require_file` / `read_file`, which
   apply the 2 MiB per-file cap. Return a JSON object for `sources` holding the
   SHA-256 of every file read; `ingest::read_with` stamps `sources.profile`
   and refuses an adapter that states a different one.
3. Add it to the static registry in `exact::ingest` and add a test that
   imports synthetic files through it and checks `sources.profile`.

Hosts always go through `ingest::read_with`, which refuses before reading:
`UNKNOWN_FILE` or `UNKNOWN_OPTION` for a name the spec does not declare,
`MISSING_FILE` or `MISSING_OPTION` for an absent required one, and
`ADAPTER_CONTRACT` for an unversioned or inconsistent profile id.

### What an adapter must refuse

An adapter maps; it does not guess. It must refuse (never coerce):

- a missing required header or column, or a header it cannot resolve by name;
- money that is not an exact decimal (floats, exponent notation, more than two
  places); amounts are never rounded;
- an unknown sign convention: the dataset is `positive_costs` (expenses
  positive, credits and contra-revenue negative) and the adapter must know,
  not infer, how the source signs map to it;
- any currency other than USD;
- vendor total, subtotal, or footer rows passed off as accounts;
- account categories it was not given by an approved mapping (use `unmapped`
  so review surfaces them, never a guessed category).

`store::import` then validates the dataset itself (`INVALID_INPUT`,
`SNAPSHOT_MISMATCH`, `ACCOUNT_MAPPING_CONFLICT`, `MONEY_OVERFLOW`), so an
adapter cannot weaken the core's checks. Paths stay a host concern: the
calculation catalog never takes a file path, and adapters run in the host
process before `import_period`'s dataset would.

The legacy standardized-CSV lanes below keep their `standardized-yardi`
lineage label (`connectors::standardized::STANDARDIZED_SOURCE_SYSTEM`); they
feed the legacy floating-point tables, not the exact-cent core.

## Standardized-CSV Post-ETL Contracts (Asset Management)

These contracts run AFTER each property's per-property ETL produces its
`Standardized/*.csv`, enforced by
`your-etl/data_validation.py` and wired into
`run_all_etls.py`. They exist to catch the class of defect where bad or MISSING
data flows silently to owner-facing RPCOE/BDDRE reports. Principle:
**fail loud / surface the gap — never silently coerce missing to zero.**

Severity: `ERROR` blocks (non-zero exit), `WARN` is suspicious-but-allowed,
`INFO` is expected-by-config.

### C1 — Rent Roll (`rent_roll.csv`) — no footer rows, sane market rent
- No row may carry a Yardi total/subtotal/footer token (`total` exact-match,
  or `all properties` / `grand total` / `subtotal` / `summary`) in any identity
  column (`unit`, `resident`, `name`, `property_name`, `unit_type`). ERROR.
- `market` (market rent) must satisfy `100 <= market <= 25,000` per unit;
  values above the band are the D1 footer-as-unit signature (e.g. $158,529).
  ERROR. Negative market rent is ERROR. (Vacant units may have market 0.)

### C2 — Concession Flow-Through (`concessions.csv` -> `tenant_profile.csv`)
- If a property's `concessions.csv` exists and carries one or more ACTIVE
  concessions (nonzero current-lease concession), the standardized
  `tenant_profile.csv` MUST report `has_active_concession` > 0. Otherwise ERROR
  (D2: broken join/merge silently zeroing concessions).
- EXCEPTION (allowlist): `EXPECTED_ZERO_CONCESSIONS = {juniper_fund}`. juniper_fund's
  third-party PM does not use ConcessionBurnOff to set active concessions, so
  0 active downstream is CORRECT and reported as INFO, never an ERROR.

### C3 — Aged Receivables Presence (`aged_receivables.csv` /
`aging_detail_summary.csv`) — missing != zero
- At least one AR feed must exist. None present -> ERROR (loud DATA-GAP).
- The chosen AR feed must carry aging-bucket columns
  (`days_31_60`, `days_61_90`, `days_over_90`). A count-only rollup
  (`snapshot_date, record_count, property_id`) has none and would masquerade as
  $0 of 31+ delinquency -> ERROR (D3).
- Buckets present but total 31+ == $0 -> WARN (a genuine all-current snapshot is
  possible, but absence-vs-zero is ambiguous; surfaced for human eyes).
- Buckets present with 31+ > 0 -> INFO (healthy).

### C4 — RPCOE Renewal-Elasticity Distribution (`tenant_profile.csv` /
report) — populated when records analyzed
- If renewal-elasticity analyzed `records_analyzed > 0`, the rent-position
  distribution `Underpriced/AtMarket/Overpriced` must NOT be `0/0/0`. Otherwise
  ERROR (D4: broken rent_gap proxy — unit-key join mismatch or a history file
  with no computable rent_gap). The validator derives the distribution from
  `tenant_profile.rent_position_cohort` (or `rent_gap` sign) so the contract is
  checkable from standardized data alone.

### C5 — Required Header Resolvable (raw Yardi parses) — fail loud on layout drift
- Raw Yardi exports (concessions, aged receivables, rent roll) are parsed by
  HEADER MEANING via `your-etl/yardi_columns.py`
  (`resolve_columns` over declarative `ColumnSchema`s), NOT by fixed column
  position. If a source is present but a REQUIRED field's header cannot be
  resolved, the parser raises `UnresolvedColumnError` and C5 turns it into an
  ERROR. This is what makes a Yardi column reorder/rename fail loud instead of
  silently mis-mapping (e.g. reading the wrong column as the concession amount).

### C1 (downstream) — footer rows must not leak into unit-keyed downstream files
- The C1 footer/total-row + market-band check is also applied to downstream
  unit-keyed files (`collections_unified.csv`, `pre_delinquency_scores.csv`),
  since a footer row cleaned from `rent_roll.csv` can survive in files the
  aggregator already produced. ERROR on any leaked footer row.

Contract set version: **v1.1 (C1–C5)**, stamped on every validated report footer.

Invocation:
```
python your-etl/data_validation.py            # all properties
python your-etl/data_validation.py --property maplewood
python your-etl/run_all_etls.py               # runs validation last
```
The validator is READ-ONLY; it never mutates `Standardized/*.csv`.
