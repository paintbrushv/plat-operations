# Assumptions

- This repository is not a Rust workspace, so Boxscore is isolated under `boxscore/`.
- Rust 2021 edition is used for broad local compatibility because Rust tooling is not installed in this environment.
- All data in `data/sample/` is fake and intentionally non-private.
- Amounts are stored as `REAL` in SQLite for v0.1 simplicity; a future production version should evaluate decimal handling.
- GL amounts use positive revenue, negative contra-revenue (concessions, bad debt), and positive expenses, so NOI is revenue minus expenses. See `docs/ISSUED_REPORTS_AND_SIGN_CONVENTION.md`.
- Budgeted occupancy is not available in v0.1 sample data, so occupancy comparisons are framed as limited-confidence signals.
- Answered operator questions create memory candidates only when the answer is specific enough to be useful.
- Capability backlog items are proposal-first and auditable; they do not modify code or deploy systems.
- External model-provider keys may be configured later, but v0.1 tests and demo do not require them.
