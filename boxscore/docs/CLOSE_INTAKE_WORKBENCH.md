# Close Intake Workbench

Milestone I-A adds a dry-run intake layer for monthly close materials.

The workbench is designed for the moment before import: files have been downloaded, but the operator has not yet decided which files are authoritative, which ones are stale, and which adapters should be trusted.

## Run

Place downloaded close materials in a private inbox such as:

```text
data/private/inbox/2026-06/
```

Then run:

```bash
cargo run -- intake scan --inbox data/private/inbox/2026-06 --period 2026-06
```

The command returns JSON and writes:

```text
reports/generated/2026-06-intake-workbench.md
```

## What It Does

- Recursively scans the inbox.
- Reads CSV headers and row counts.
- Classifies likely source type.
- Infers likely period and property when possible.
- Flags period mismatches and unsupported files.
- Produces a dry-run import plan and close package checklist.

## Recognized Source Types

- Financial actual/budget candidates:
  - budget-vs-actual
  - budget variance
  - budget comparison
  - income statement
  - trial balance
  - CSVs with actual and budget headers
- Rent roll
- Delinquency / aged receivables
- Leasing / traffic
- Collections
- RPCOE weekly reports
- BDDRE weekly reports

## Safety Rules

- The workbench does not import data.
- The workbench does not delete, move, or modify downloaded files.
- The workbench does not transmit private data.
- CSV profiling is shallow: headers, row counts, and a small sample are used only for classification hints.
- Resident/tenant-level files should remain in `data/private/`.

## Product Role

The intake workbench prepares Milestone I by making June close materials easy to triage. Once the operator chooses authoritative June financial sources, Boxscore can add targeted adapters instead of guessing from a pile of files.
