# Boxscore Implementation Tracker

This tracker records the milestone spine for the local-first Boxscore Rust harness.

## Completed

- [x] Milestone A: Product-grade Rust harness base with SQLite schema, CLI, API, reports, tools, gaps, questions, memories, and deterministic model seam.
- [x] Milestone B: Standardized source registry and GL variance ingestion from existing Yardi-derived CSV lanes.
- [x] Milestone C: Operating snapshots ingestion for rent roll, delinquency, and leasing context.
- [x] Milestone D: Sanitized fixtures plus owner-ready variance report upgrades.
- [x] Milestone E: Portfolio-level operating demo with asset-manager review index.
- [x] Milestone F: Collections and bad-debt bridge for June 2026 weekly operating context.
- [x] Milestone G: Account mapping review acceleration with CSV export/import, approved mapping memories, and reclassification of existing unmapped GL rows.
- [x] Milestone H: Period alignment and June close readiness with owner-report blockers, stale-feed warnings, and BDDRE/RPCOE weekly context checks.
- [x] Milestone I-A: Close intake workbench with private inbox scan, dry-run source classification, period mismatch warnings, and close package checklist.

## Milestone G Details

- [x] Export suggested and unmapped standardized Yardi account mappings to `data/review/account_mappings.csv`.
- [x] Import only explicitly approved review rows.
- [x] Require `reviewed_category` for approved rows.
- [x] Upsert approved rows into `account_mappings`.
- [x] Create durable `account_mapping` memories for approved rows.
- [x] Reclassify existing unmapped GL actual/budget rows after approval.
- [x] Preserve non-approved rows for future operator review.
- [x] Document the CSV contract and portfolio review loop.
- [x] Add regression tests covering export, import, validation, memory creation, and reclassification.

## Milestone H Details

- [x] Add `boxscore close-readiness --period <YYYY-MM>`.
- [x] Assess Actual GL, Budget GL, rent roll, delinquency, leasing, and collections by selected period.
- [x] Mark missing required feeds as blockers and stale required feeds as warnings.
- [x] Generate property-level operator questions for stale or missing close feeds.
- [x] Write a markdown close-readiness report under `reports/generated/`.
- [x] Detect built-in lane BDDRE/RPCOE weekly report availability as supporting context.
- [x] Add regression tests for ready, stale/missing, and portfolio rollup behavior.

## Recommended Next Milestone

- [ ] Milestone I: June GL source expansion and close package assembly.

Milestone I should use the intake workbench output to pick authoritative June 2026 financial sources when `budget_comparison.csv` is stale: trial balance, income statements, budget-vs-actual exports, or standardized GL transaction summaries. The output should be a June close package that can move properties from close-readiness blockers into owner-ready variance analysis.

## Milestone I-A Details

- [x] Add `boxscore intake scan --inbox <path> --period <YYYY-MM>`.
- [x] Recursively scan close-material folders without importing data.
- [x] Classify financial actual/budget candidates, operating files, collections, BDDRE weekly reports, RPCOE weekly reports, and unsupported files.
- [x] Infer likely property and period from filename/header samples.
- [x] Flag period mismatches and unsupported extensions.
- [x] Write a markdown dry-run import plan and close package checklist.
- [x] Document safety guardrails: no imports, no file modification, no external transmission.
