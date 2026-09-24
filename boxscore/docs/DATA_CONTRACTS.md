# Data Contracts

All v0.1 ingestion uses CSV files with headers.

## Properties

`name,market,unit_count,owner_entity,property_manager`

## GL Actuals And Budgets

`property,period,account_code,account_name,category,amount`

- `property` must match an ingested property name.
- `period` must be `YYYY-MM`.
- `amount` is numeric.
- Revenue is positive.
- Contra-revenue and expenses are negative.

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
