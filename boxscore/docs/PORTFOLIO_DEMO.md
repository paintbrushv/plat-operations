# Portfolio Demo

Milestone E turns Boxscore from verified components into a repeatable portfolio-level operating demo.

The demo:

- initializes a local SQLite database
- ingests standardized GL actual/budget data for the built-in property lanes
- ingests standardized rent roll, delinquency, and leasing snapshots
- ingests standardized collections context
- selects the latest period with both actuals and budgets for each property
- runs variance analysis for each property
- writes one markdown report per property
- writes a portfolio index for asset-manager review
- surfaces unmapped accounts, gaps, questions, and capability backlog counts

## Run

```bash
cd boxscore
./scripts/portfolio_demo.sh
```

Optional environment variables:

```bash
export DATABASE_URL='sqlite://data/boxscore_portfolio_demo.db?mode=rwc'
export BOXSCORE_REPORT_DIR='reports/generated/portfolio-demo'
export BOXSCORE_KEEP_DEMO_DB=1
```

## One-Shot CLI

If the database is already initialized or you want to control the DB lifecycle yourself:

```bash
cargo run -- demo portfolio
```

## Review Outputs

Start with:

```text
reports/generated/portfolio-demo/portfolio-demo-index.md
```

Then review each property report linked in the index.

## Current Real-Lane Caveat

Current standardized GL bridge files are March 2026, while current operating and collections context files are mostly June 2026. Boxscore does not silently attach June operating or collections data to March GL variance. The demo therefore preserves the mismatch as missing period-matched operating evidence in the March property reports.

That is intentional product behavior: the harness should reveal source coverage gaps instead of inventing operating causality.

## Asset Manager Review Loop

After running the demo:

1. Review the portfolio index for missing source coverage and low-confidence reports.
2. Run `cargo run -- accounts unmapped` to see accounts that still landed outside the ontology.
3. Run `cargo run -- accounts export-review --file data/review/account_mappings.csv` and have an operator approve recurring mappings in the CSV.
4. Run `cargo run -- accounts import-review --file data/review/account_mappings.csv` to persist approved mappings and mapping memories.
5. Run `cargo run -- close-readiness --period 2026-06` before owner reporting to see which June feeds are current, stale, or missing.
6. Review `cargo run -- questions list` for operator follow-up.
7. Review `cargo run -- capabilities list` to see what repeated data gaps are teaching the roadmap.
8. Re-run the demo after mapping and data-quality fixes to compare confidence.
