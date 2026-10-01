# Boxscore

**The operating intelligence layer for multifamily portfolios.**

Boxscore is a local-first Rust harness for multifamily operating intelligence. The first wedge is **Boxscore Ops v0.1: Multifamily Ops Variance Intelligence**: a narrow analyst that explains why NOI missed or beat budget, what operating signals may explain the movement, what evidence supports the conclusion, and what data is still missing.

## What It Is

- A product-grade harness around models, data, memory, tools, permissions, observability, and domain workflows.
- A local-first variance analyst for GP, asset-manager, and regional-operator workflows.
- A structured learning loop that records gaps, questions, memories, evidence, tool failures, and capability proposals.

## What It Is Not

- Not a chatbot wrapper.
- Not an autonomous code-writing loop.
- Not a real Yardi, Entrata, banking, investor, or tenant-data connector.
- Not a SaaS auth/billing product yet.
- Not allowed to silently self-modify production code, delete data, transmit private data externally, or deploy anything.

## Quick Start

Rust is required.

```bash
cd boxscore
cp .env.example .env
cargo run -- init
cargo run -- ingest property --file data/sample/properties.csv
cargo run -- ingest gl-actuals --file data/sample/gl_actuals.csv
cargo run -- ingest gl-budgets --file data/sample/gl_budgets.csv
cargo run -- ingest rent-roll --file data/sample/rent_roll_snapshots.csv
cargo run -- ingest delinquency --file data/sample/delinquency_snapshots.csv
cargo run -- ingest leasing --file data/sample/leasing_snapshots.csv
cargo run -- analyze variance --property "Oak Ridge" --period "2026-05"
```

Or run the demo:

```bash
cd boxscore
./scripts/demo.sh
```

Run the standardized portfolio demo:

```bash
cd boxscore
./scripts/portfolio_demo.sh
```

The portfolio demo ingests the built-in standardized property lanes, including GL, operating snapshots, and collections context. It runs one variance report per property for the latest period with both actuals and budgets, and writes an asset-manager review index under `reports/generated/portfolio-demo/portfolio-demo-index.md`.

The isolated [synthetic September handoff rehearsal](docs/SYNTHETIC_SEPTEMBER_HANDOFF.md) exercises versioned invented PMS layouts and a frozen correction snapshot. It does not close a real property period.

[Variance report history and demo expense signs](docs/ISSUED_REPORTS_AND_SIGN_CONVENTION.md) explains the unique issued reports, the synthetic close report link, and the corrected public sample expense convention.

The [September 2026 source intake](docs/SEPTEMBER_2026_SOURCE_INTAKE.md) lists the approved export and grant evidence needed before real TC or CCAR adapters can be validated.

Run a close-readiness view for a selected period:

```bash
cd boxscore
cargo run -- close-readiness --period 2026-06
```

The close-readiness report shows which properties have period-matched GL, operating snapshots, collections context, and supporting weekly BDDRE/RPCOE reports before an owner-ready variance narrative is published.

Scan a private close-materials inbox before importing:

```bash
cd boxscore
cargo run -- intake scan --inbox data/private/inbox/2026-06 --period 2026-06
```

The intake workbench classifies likely close files, flags period mismatches, and writes a dry-run import plan without modifying analytical tables.

## Key Commands

```bash
boxscore init
boxscore ingest property --file data/sample/properties.csv
boxscore ingest gl-actuals --file data/sample/gl_actuals.csv
boxscore ingest gl-budgets --file data/sample/gl_budgets.csv
boxscore ingest rent-roll --file data/sample/rent_roll_snapshots.csv
boxscore ingest delinquency --file data/sample/delinquency_snapshots.csv
boxscore ingest leasing --file data/sample/leasing_snapshots.csv
boxscore intake scan --inbox data/private/inbox/2026-06 --period 2026-06
boxscore analyze variance --property "Oak Ridge" --period "2026-05"
boxscore questions list
boxscore questions answer --id <id> --answer "Concessions were approved for lease-up."
boxscore gaps list
boxscore capabilities list
boxscore memories
boxscore accounts unmapped
boxscore accounts export-review --file data/review/account_mappings.csv
boxscore accounts import-review --file data/review/account_mappings.csv
boxscore tools
boxscore demo portfolio
boxscore close-readiness --period 2026-06
boxscore ingest-standardized collections --all
boxscore server

# Call Ledger — record, score, and review the harness's predictions/recommendations
boxscore calls record --property "maplewood" --call-type delinquency_risk --origin-period 2026-05 --mature-by 2026-06 --confidence 0.72 --payload '{"resident_code":"T-00412","probability":0.72,"window_days":30}'
boxscore calls list
boxscore calls score --period 2026-05
boxscore calls track-record
boxscore calls track-record --property "maplewood" --call-type noi_diagnosis
```

## API

Start:

```bash
cargo run -- server
```

Endpoints:

- `GET /health`
- `GET /properties`
- `GET /task-runs`
- `GET /task-runs/:id`
- `POST /analyze/variance`
- `GET /gaps`
- `GET /questions`
- `POST /questions/:id/answer`
- `GET /capabilities`
- `GET /memories`

Example:

```bash
curl -X POST http://127.0.0.1:3818/analyze/variance \
  -H 'content-type: application/json' \
  -d '{"property":"Oak Ridge","period":"2026-05"}'
```

## How The Harness Learns

Each analysis records task runs, tool runs, evidence, gaps, questions, memories, and capability proposals. The system improves by accumulating a structured operating backlog, not by silently changing code.

## Current Limitations

- Uses fake sample CSV data only.
- Uses SQLite by default.
- No external model provider required or called.
- No frontend yet.
- No real property-management-system connectors yet.
- No resident-level data model; future versions should minimize and redact PII.
- v0.1 has been verified locally with `cargo fmt -- --check`, `cargo check`, `cargo test`, and `cargo clippy --all-targets --all-features`.
- Current real standardized GL files are March 2026 while current operating and collections context files are mostly June 2026; Boxscore does not silently attach June operating data to March variance reports.
- Account mapping review is CSV-based; there is not yet a reviewer UI or approval workflow with roles.
- Close readiness reports current/stale/missing feeds, but does not yet ingest June GL from trial balance or income statement alternatives when `budget_comparison.csv` remains stale.

## Roadmap

- Occupancy budget import.
- Unit-level rent roll parser.
- Yardi GL budget and actual parser.
- Account mapping UI with role-aware review history.
- Make-ready aging integration.
- Owner-report memo generator.
- Weekly leasing velocity dashboard.
- Bank balance and cash forecast connector.
- Controllable-expense anomaly detection.
- Lender/investor surveillance reporting.
