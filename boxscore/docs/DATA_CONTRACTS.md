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

The catalog's `import_period` (1.1.0 and later) runs a registered adapter too,
on file contents sent inline instead of paths: `profile`, `files` (adapter file
name to that file's full UTF-8 text as a JSON string) and `options`, in place
of `dataset`.

```json
{"profile": "canonical.csv/1",
 "files": {"actuals": "account_code,account_name,category,amount\n4000,Gross Potential Rent,rental income,36000.00\n…",
           "budgets": "account_code,account_name,category,amount\n…",
           "snapshot": "{\"as_of_date\": \"2026-05-31\", …}"},
 "options": {"property": "Sample Gardens", "period": "2026-05", "units": "24"}}
```

In memory the files are `AdapterInputs::contents` (`with_contents`), which
`read_file` returns ahead of any path-backed file of the same name. Each inline
file is capped at the same 2 MiB as a file on disk, and all inline files
together at 2 MiB (`INPUT_LIMIT`). The adapter's own `sources` are identical
either way (they hash the bytes), so the revision's `input_sha256` and per-file
hashes match an `import-with` of the same files; `ingest::read_with` adds
`sources.files`, one `{"name", "sha256", "bytes", "inline": true}` per inline
file and no path. An adapter must therefore not state `sources.files` itself
(`ADAPTER_CONTRACT` when inline files are present).

`canonical.csv/1` ignores a leading UTF-8 byte-order mark (`EF BB BF`) on
`actuals` and `budgets` when it parses. The per-file hash it records, and the
inline `files[].sha256`, are of the bytes as received with the mark included, so
provenance matches the file on disk; the dataset-level `input_sha256` hashes the
parsed dataset and is therefore the same with or without the mark. A second
leading mark is refused (`INVALID_INPUT`, naming U+FEFF), since the CSV reader
would otherwise drop it silently.

`sources.supplied` is the caller's and every other key is the server's.
`import_period` records a caller's own `sources` object under
`sources.supplied` in both forms, and never anywhere else, so a caller cannot
write `profile`, `files` or a hash the server records. The dataset form
records `{"canonical_json_sha256": "<SHA-256 of the canonical dataset>"}`
plus `supplied` when the caller sent `sources`. An adapter must not state
`sources.supplied` (`ADAPTER_CONTRACT`). Anything under `supplied` is stored
as sent and is not verified.

### Shipped profiles

| profile | files | options | `sources` recorded |
|---|---|---|---|
| `canonical.csv/1` | `actuals`, `budgets` (header `account_code,account_name,category,amount`); optional `snapshot` (JSON object) | `property`, `period` (`YYYY-MM`), `units` (whole number) | `profile`, `actuals_sha256`, `budgets_sha256`, `snapshot_sha256` (null when absent); plus `files` for inline contents |
| `canonical.json/1` | `input` (canonical period dataset JSON) | none | `profile`, `canonical_json_sha256`; plus `files` for inline contents |

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
adapter cannot weaken the core's checks. That validation refuses a `property`,
`account_code`, `account_name` or `category` containing a Unicode control
character (`char::is_control`), an invisible format character (category Cf,
for example U+200B to U+200F, U+2028 to U+202E, U+2060 to U+2064, U+FEFF), an
invisible filler or variation selector (U+034F, U+115F, U+1160, U+180B to
U+180F, U+2800, U+3164, U+FE00 to U+FE0F, U+FFA0) or any whitespace other than a
plain space (U+00A0, U+2000 to U+200A, U+202F, U+205F, U+3000, the line
separators), naming the row index, the field and the code point (the property
has no row: a hidden character there would fork the revision chain). The rule
is for new imports; a revision stored before it stays reviewable
(`review_period` flags it with the review reason `invisible_characters`), and a
reviewed copy migration copies such legacy rows unchanged, lists them in the
plan as an `invisible_characters` difference and does not stop for them. Paths stay a host concern: the
calculation catalog never takes a file path. Adapters run in the host process
on local files (`import-with`), or inside `import_period` on inline contents.

### Inspecting an export

Before anyone proposes a mapping for a vendor export, the catalog's stateless
`inspect_export` (1.0.0) describes its shape: `{"name", "text", "redact"}` in,
and out the dialect (delimiter, quote, line ending, byte-order mark), the header
row, the banner rows above it and the footer rows below it (total, subtotal and
similar tokens, by the same rule as the C1 footer check below), the data row
count, and for each column (at most 256) a kind (`money`, `integer`, `date`,
`period`, `empty`, `text`), a fill rate, a distinct count and up to five sample
values. It never returns the whole file. With redaction on (the default), a
column is redacted, its sample replaced by `["<redacted>"]` and listed by
`{index}` in `redaction.columns`, when its header names a person, contact or
account whatever the column's kind: `resident`, `tenant`, `lessee`, `occupant`,
`applicant`, `guarantor`, `email`, `phone`, `ssn`, `contact`, `address`,
`social`, `birth`, `card`, `bank`, `routing`, `license`, `licence`, `mobile`,
`customer`, `payee`, `owner`, `holder`, `household` (any case, as a word or part of one),
`ss`, `tin`, `dob`, `tel`, `cell`, `mail`, `first`, `last`, `full`,
`tax id` (whole words, camel case counted as words, so `Hotel` stays visible,
and so does a `Lease Start` date), or `name` anywhere in the header unless
every `name` directly follows `account`, `gl`, `vendor`, `property`, `unit`,
`column` or `report` (spaces, `_` or `-` allowed between; camel case too) and no
person word is present (`Vendor Name` and `AccountName` stay visible, `Name` and
`Name / Unit` are redacted). A text or integer column with an `@`, a `ddd-dd-dddd` value or
a 13 to 19 digit card number in any row is redacted whatever its header. A text
column (or an integer column with a value of 7 or more digits) of at least 8
values that are mostly different (more than half distinct, or at least 10
distinct values each repeated at most three times) is redacted too, unless its
header has an identifier or label word (`code`, `id`, `number`, `num`, `no`,
`ref`, `#`, `account`, `unit`, `description`, `category`); that exemption never
applies with a person word or an unqualified `name`, and a qualified name
header has it, so `account_name`, unit ids and account descriptions stay
visible: a mapping needs them. The header row is echoed, so it is checked too: a
single cell with `@`, mostly digits (a year or `YYYY-MM` is a label) or more
than 40 characters is replaced by `<redacted header N>`, and `header_redacted`
says so. A banner above the header, even a two-cell one such as `Run Date,
10/16/2026`, does not redact it. A header with fewer cells than its data (more
than half as many) is chosen over the first data row when the values below are
all text, and its missing names are `<unnamed N>`.
`redact: false` turns all of it off. Every list is capped (banner rows 10,
footer rows 20, samples 5 of at most 80 characters), a control, invisible or
non-space whitespace character in anything echoed is written `U+XXXX` before
the caps apply, and the serialized result is held to 256 KiB (samples dropped
first, then banner rows, then names shortened, `truncated: true`), so the result
is bounded in bytes whatever the input. A row of more than 256 cells refuses
`INPUT_LIMIT` as it is read. The agent sees only this result, not the
file, and an `inspect_export` trace never keeps its input even with
`BOXSCORE_TRACE_INPUTS=1`; if the caller sends the text in a tool call, the text itself passes
through the client like any tool argument (see `docs/DATA_FLOW.md`, section 5).

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
