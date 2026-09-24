# Onboarding a Property into Boxscore

This is the one-page sheet to hand a prospect's analyst. It says exactly **what
to export from Yardi**, **what columns each file needs**, **where to drop the
files**, and **the single command to load it**. Once these files are in place,
Boxscore renders your own close — GL variance, rent roll, delinquency, renewals,
the NOI bridge, and the portfolio band — against your data, locally.

Boxscore is **local-first**: your files stay on your machine. Nothing is
uploaded. Onboarding registers a *lane* (one property) in a local `lanes.json`
and ingests its CSVs into a local SQLite database.

---

## 1. Folder layout

Create one folder for the property with a `Standardized/` subfolder. Drop the
exports there:

```
<property-folder>/
└── Standardized/
    ├── budget_comparison.csv     # GL actual vs budget by account/period (required)
    ├── gl_transactions.parquet   # optional; powers the Ledger transaction drill-down
    ├── chart_of_accounts.csv     # account code → name → category map
    ├── rent_roll.csv             # current rent roll snapshot
    ├── aged_receivables.csv      # delinquency / aging by resident
    ├── leasing_funnel.csv        # weekly leasing activity (shows/apps/approvals)
    ├── collections_unified.csv   # collections + risk roll-up
    ├── lease_expirations.csv     # upcoming lease expirations
    ├── concessions.csv           # concession detail
    └── weekly_activity.csv       # weekly move-in/out/notice activity
```

You do **not** need every file to get value. At minimum, supply
`budget_comparison.csv` (drives the GL variance + NOI bridge). Each additional
file lights up another screen. Missing files are reported, not errors — Boxscore
onboards whatever is present.

> **Operating snapshots load as a group.** `rent_roll.csv`, `aged_receivables.csv`,
> and `leasing_funnel.csv` ingest in a single step — if any one of the three is
> missing, that whole step is skipped. Supply all three together to light up
> occupancy, delinquency aging, and the leasing funnel at once.

> **Period format everywhere is `YYYY-MM`.** Dates are ISO `YYYY-MM-DD`.
> Currency is numeric (no `$`, no commas). Column names are `snake_case`.

---

## 2. What to export from Yardi, file by file

These are tidy, "standardized" CSVs. If your team already produces a Yardi
"Budget Comparison", "Rent Roll", "Aged Receivables", or "Box Score" export,
rename the columns to match below (or ask us — we have ETL that maps the common
Yardi raw exports to these).

### `budget_comparison.csv` — GL actual vs budget *(required)*
The financial spine. One row per account per period.

| column | meaning |
|--------|---------|
| `account_code` | GL account number |
| `description` | account name |
| `ptd_actual` | period-to-date actual amount |
| `ptd_budget` | period-to-date budgeted amount |
| `period` | `YYYY-MM` |
| `property_id` | your Yardi property id (the same id you pass as `--property-id`) |

*Yardi source:* Financial Analytics → **Budget Comparison** (or the monthly
income-statement export), pivoted long so each account/period is a row.

### `chart_of_accounts.csv` — account map
| column | meaning |
|--------|---------|
| `account_code` | GL account number |
| `account_name` | human name |
| `category` | NOI category (Revenue / Operating Expense / etc.) |

*Yardi source:* **Chart of Accounts** export. Lets Boxscore classify each account
for the NOI bridge and variance.

### `rent_roll.csv` — current rent roll
| column | meaning |
|--------|---------|
| `unit` | unit label |
| `unit_type` | floorplan |
| `sqft` | square feet |
| `resident` | resident code |
| `name` | resident name |
| `market` | market rent |
| `charge_rent` | in-place / charged rent |
| `property_id` | Yardi property id |
| `snapshot_date` | `YYYY-MM-DD` of the snapshot |

*Yardi source:* **Rent Roll** (current). Drives occupancy and the renewals
spread (market vs in-place).

### `aged_receivables.csv` — delinquency / aging
| column | meaning |
|--------|---------|
| `property_id`, `resident_code`, `resident_name`, `resident_status` | resident identity / status |
| `current_owed` | currently due |
| `days_0_30`, `days_31_60`, `days_61_90`, `days_over_90` | aging buckets |
| `prepayments` | prepaid balance (shows as green) |
| `total_owed`, `total_delinquent` | totals |
| `snapshot_date` | `YYYY-MM-DD` |

*Yardi source:* **Aged Receivables / Delinquency** report. Drives the
Delinquency screen, worst-payer-first with real aging buckets.

### `leasing_funnel.csv` — weekly leasing activity
| column | meaning |
|--------|---------|
| `prospect_id` | prospect identifier |
| `event_date`, `week_start`, `week_end` | dates (`YYYY-MM-DD`) |
| `shows`, `applications`, `approvals` | funnel counts |
| `property_id` | Yardi property id |

*Yardi source:* **Box Score / Leasing** weekly activity.

### `collections_unified.csv` — collections + risk roll-up
Per-unit collections detail with risk tiers (delinquency tier, pre-delinquency
score, on-time %, pricing opportunity). Key columns: `property_id`, `unit`,
`resident_code`, `resident_name`, `total_delinquent`, `delinquency_tier`,
`pre_delinquency_score`, `on_time_pct`, `pricing_opportunity`, `snapshot_date`.

*Yardi source:* derived from collections + receivables; ask us for the mapping
if your team doesn't already produce it.

### `lease_expirations.csv`, `concessions.csv`, `weekly_activity.csv`
Optional detail files for the renewals funnel and weekly activity views. Export
the corresponding Yardi reports and drop them in with `snake_case` headers.

---

## 3. The one command

From the `boxscore/` directory:

```bash
boxscore onboard \
  --name "Bagholder Capital" \
  --property "Vantage at Yieldmore" \
  --dir /path/to/property-folder \
  --property-id s101 \
  --units 220
```

- `--name` — the GP / owner display name.
- `--property` — the property display name (a lane key is slugged from it, e.g.
  `vantage-at-yieldmore`; override with `--key`).
- `--dir` — the folder containing `Standardized/`.
- `--property-id` — your Yardi property id(s) used to attribute GL rows. Repeat
  the flag for multiple ids.
- `--units` — total unit count (optional hint).

This registers the lane in `lanes.json` (created on first onboard; kept local
and gitignored) and ingests every Standardized CSV that is present. It prints a
summary listing what ingested, what was skipped, and which expected files were
missing.

Then open the desk:

```bash
boxscore desk
```

Your property now appears alongside any existing lanes. Re-running `onboard` (or
re-dropping refreshed CSVs and re-running) is idempotent — it updates in place,
it does not duplicate.

---

## 4. The per-unit intelligence panes (BDDRE / RPCOE)

The Delinquency **risk watchlist** and the Renewals **recommendations** panes
(recommended rent, confidence, days-to-expiration, drivers) are produced by
Boxscore's weekly engine — they read `Reports/bddre_*.csv` and
`Reports/rpcoe_recommendations.csv` under the property's folder. Those light up
once you run the weekly-ops pass on the new lane (see the `weekly-ops` /
`pm-weekly-report` skills). The financial, occupancy, delinquency-aging, and
NOI-bridge screens all work from the Standardized CSVs above with no engine run
required.

---

## 5. Backward compatibility

If no `lanes.json` exists, Boxscore runs with its three built-in lanes exactly
as before. Onboarding only *adds* configured lanes on top of the built-ins; it
never removes or alters them.
