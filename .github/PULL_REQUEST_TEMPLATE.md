## What & why

One bounded slice: what does this change do, and what analysis/workflow does it serve?

## Grounding

Does this touch financial math or material claims? If yes, show the tests that tie
conclusions to source data.

## Verification (run from `boxscore/`)

```text
cargo check
cargo clippy --all-targets -- -D warnings
cargo test
cargo fmt --check
```

## Data hygiene

- [ ] No real operating data, tenant/PII data, or secrets added — all fixtures synthetic
- [ ] No new f64 money arithmetic (or flagged with rationale)
- [ ] No new non-test unwrap()/expect()
- [ ] No new dependencies / unsafe (or named for sign-off)
