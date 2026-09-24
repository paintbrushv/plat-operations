# Contributing to plat-operations (BOXSCORE)

Thanks for contributing! This crate is a financial-analysis harness, so a few
rules matter more than usual:

## Ground rules

1. **Never commit real operating data.** All fixtures and samples are synthetic
   (`Property_A`, `Resident A`, placeholder dollars). If you add a fixture, make
   it up — never paste real GL, rent-roll, receivables, or resident data.
2. **Money is never `f64`.** Today the codebase violates this (documented); do not
   add NEW `f64` money arithmetic without flagging it in your PR.
3. **No new non-test `unwrap()`/`expect()`** — return `Result`.
4. **No new dependencies and no `unsafe`** without naming them and getting sign-off.
5. **Don't edit an applied migration** — add a new one.

## Definition of done

Behavior tested (financial math especially) · claims source-grounded with
confidence labels · docs updated when behavior changes · verify-commands pass
with shown output:

```bash
cd boxscore
cargo check
cargo clippy --all-targets -- -D warnings
cargo test
cargo fmt --check
```

## Commit style

Conventional commits (`feat(scope): ...`, `fix(scope): ...`). One bounded slice
per PR — no unrelated refactors.

## Reporting bugs

Open an issue with the command you ran, the output, and (if relevant) a
synthetic reproducer. Do not paste real property, tenant, or vendor data into
issues.