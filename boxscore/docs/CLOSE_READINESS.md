# Close Readiness

Milestone H adds a period-alignment view for monthly close and owner-report preparation.

The command answers:

- Which properties can support an owner-ready variance narrative for a selected period?
- Which required feeds are current, stale, or missing?
- Which questions should an asset manager ask before publishing the story?
- Which BDDRE/RPCOE weekly reports are available as supporting context?

## Run

```bash
cargo run -- close-readiness --period 2026-06
```

The command reads the local Boxscore SQLite database and writes:

```text
reports/generated/2026-06-close-readiness.md
```

## Required Feeds

Owner-ready status requires period-matched:

- Actual GL
- Budget GL
- Rent roll
- Delinquency
- Leasing
- Collections

Boxscore treats stale required feeds as warnings and missing required feeds as blockers. A property is only owner-ready when every required feed is current for the selected period.

## Weekly Context

For built-in standardized lanes, Boxscore also checks the asset `Reports/` folder for:

- `RPCOE_Weekly_Report_<date>.md`
- `BDDRE_Weekly_Report_<date>.md`

These files are supporting context, not substitutes for period-matched GL and operating snapshots. A June 2026 RPCOE or BDDRE weekly report can help explain operating risk, but it should not be presented as proof of a June NOI variance without June GL and snapshot coverage.

## Close Run Order

After ingest and a passing close-readiness check, score the previous period's calls and refresh the track record before publishing owner-facing reports:

```bash
# 1. Refresh standardized feeds for the period.
boxscore ingest-standardized gl --all
boxscore ingest-standardized ops --all
boxscore ingest-standardized collections --all

# 2. Confirm all required feeds are present and current.
boxscore close-readiness --period <YYYY-MM>

# 3. Mature and score last period's calls; refresh the batting-average track record.
boxscore calls score --period <YYYY-MM>

# 4. Inspect the track record before publishing.
boxscore calls track-record
```

Step 3 is freshness-gated: if the required actuals have not landed yet, open calls stay open and are retried on the next invocation. Running `calls score` before `close-readiness` passes is safe — it will simply score nothing until actuals are present.

## Interpretation Guardrail

The close-readiness report is intentionally conservative. It should block owner-ready language when required feeds are missing or stale, and it should turn those gaps into concrete operator questions instead of inventing causality.
