# plat-operations (BOXSCORE)

**Local-first, AI-native multifamily asset-management intelligence harness** — the Rust
crate lives in [`boxscore/`](boxscore/). It ingests standardized multifamily operating
data (GL, budgets, rent roll, receivables, leasing, monthly actuals) into SQLite,
computes NOI variance / T12 reversion / close-readiness / a scored call-ledger, and
outputs grounded intelligence as markdown reports, an HTTP/JSON API, a terminal UI,
and natural-language answers.

It behaves like a sharp, skeptical multifamily asset-management analyst — **not** a
chatbot, a dashboard toy, or a reporting generator.

## What's in this repository

| Path | Contents |
|---|---|
| [`boxscore/`](boxscore/) | The Rust crate: ingestion connectors, variance/T12/close-readiness engine, call ledger, TUI, HTTP API, `ask` agent |
| [`boxscore/docs/`](boxscore/docs/) | Architecture, data contracts, ontology, security & privacy docs |
| [`boxscore/data/sample/`](boxscore/data/sample/) | **Synthetic** sample portfolio used by the demo scripts |
| [`boxscore/tests/fixtures/`](boxscore/tests/fixtures/) | **Synthetic** standardized CSV/parquet fixtures used by the test suite |
| [`boxscore/demo/`](boxscore/demo/) | Synthetic demo-data generators and seed scripts |

All sample data and fixtures in this tree are **synthetic**. No real portfolio,
tenant, vendor, or investor data is included.

## Quick start

Requires Rust (stable).

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

Or run the demo: `./scripts/demo.sh` — then `./scripts/portfolio_demo.sh` for the
multi-property standardized-lane demo, and `cargo run -- close-readiness --period 2026-06`.

## Verification

From `boxscore/`:

```bash
cargo check
cargo clippy --all-targets -- -D warnings
cargo test
cargo fmt --check
```

## Honesty section (read before adopting)

- **Money is currently `f64`/`REAL` internally.** This is a known, documented
  violation of the project's own engineering rules; migration to fixed-point is a
  domain decision, not done. See `boxscore/ASSUMPTIONS.md`.
- **Validation is synthetic-only.** The bundled fixtures and demo data are
  fabricated; no real portfolio data has been used to validate this public tree.
- **No production connectors.** Not a real Yardi/Entrata/banking connector; the
  standardized-CSV contract is documented in `boxscore/docs/DATA_CONTRACTS.md`.
- **Grounded-or-it-doesn't-ship.** Every material claim in a generated report is
  meant to carry source, logic, assumptions, a confidence label, and a recommended
  next action; if the data doesn't support a conclusion, it becomes a question.
- **Human-in-the-loop.** Investor/lender reports, PM-performance criticism, and
  fraud/waste claims are never auto-sent.

## License

Apache-2.0 — see [LICENSE](LICENSE).