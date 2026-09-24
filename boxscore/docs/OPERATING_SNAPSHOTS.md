# Operating Snapshot Ingestion

Milestone C imports property-level operating snapshots from existing standardized Yardi-derived CSVs.

The connector is intentionally aggregate-first. It may read resident-level source rows, but Boxscore v0.1 persists only property-level snapshot totals, source file lineage, row-count evidence, gaps, and operator questions.

## Sources

- `Standardized/rent_roll.csv`
- `Standardized/aged_receivables.csv`
- `Standardized/leasing_funnel.csv`

## Rent Roll Rules

- Occupied units are counted when `resident` or `name` is non-empty and not a vacancy marker.
- Vacant units are counted when resident/name fields are empty or contain markers such as `VACANT`, `available`, `model`, `admin`, or `down`.
- Down-unit markers increase `down_units`, but no resident/name value is retained.
- Market rent total uses the `market` column where present.
- In-place rent currently uses `charge_rent` only.
- Other recurring charges such as pest, trash, pet rent, parking, RUBS, MTM, washer/dryer, or utilities are not included in in-place rent until account-level charge inclusion rules are approved and documented.
- If the rent roll lacks `snapshot_date` or `as_of_date`, Boxscore uses the latest recognizable RentRoll raw file date as a fallback and creates a `missing_snapshot_date` gap.

## Delinquency Rules

- Delinquent amount uses `total_delinquent` when available.
- If `total_delinquent` is absent, Boxscore falls back to `days_0_30 + days_31_60 + days_61_90 + days_over_90`.
- Delinquent units/residents count rows with positive delinquency exposure.
- Prepayments are aggregated as absolute values from `prepayments`.
- Resident names and resident codes are not persisted.

## Leasing Rules

- `shows` maps to Boxscore `tours` until a cleaner tours field exists.
- `applications` and `approvals` map directly.
- `week_end` is used as the snapshot date, taking the latest week in the source file.
- Missing `leads`, `move_ins`, or `move_outs` create `missing_leasing_field` gaps and operator questions.
- Missing fields are not treated as proven zero unless a future source contract says they are semantically zero.

## CLI

```bash
cargo run -- ingest-standardized ops --lane willow-brook
cargo run -- ingest-standardized ops --all
```

## Privacy Contract

Boxscore does not persist resident names, resident codes, prospect names, or tenant-level balances in operating snapshot tables. Future unit-level or tenant-level features must use explicit privacy review, redaction rules, and a separate schema.
