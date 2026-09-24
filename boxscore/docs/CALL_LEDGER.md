# Call Ledger

The Call Ledger is Boxscore's mechanism for making explicit, falsifiable positions and scoring them against later actuals. Every prediction the harness emits and every recommendation a skill records lands here. The system scores each one when the target period closes, computes a batting average per property and call type, and injects the running track record into the model's system prompt so confidence is weighted by demonstrated accuracy.

## Why "Call"

A call is a scored, falsifiable commitment — a position the harness takes at a specific point in time that can be verified later. The term was chosen over "prediction" (too narrow — recommendations are also calls) and "claim" (too loose — a claim does not carry a score). The user-facing artifact is the track record: the batting average that shows whether the harness was right.

## Tables

### `calls` (migration `005_calls.sql`)

| Column | Type | Description |
|---|---|---|
| `id` | TEXT PK | UUID |
| `property_id` | TEXT | Foreign key into `properties` |
| `origin_period` | TEXT | YYYY-MM when the call was made |
| `call_type` | TEXT | `noi_diagnosis`, `delinquency_risk`, `renewal_rec`, or `t12_reversion` |
| `status` | TEXT | `open` or `scored` |
| `made_at` | TEXT | ISO-8601 timestamp |
| `mature_by` | TEXT | YYYY-MM when the call can be scored (always `origin_period + 1 month`) |
| `confidence` | REAL | Optional 0–1 caller confidence (used in calibration gap) |
| `payload_json` | TEXT | Domain-specific JSON — see shapes below |
| `outcome_json` | TEXT | Written by scorer when status flips to `scored` |
| `score` | REAL | 0.0–1.0 accuracy score (Brier-style for probabilistic calls; graded for reversion) |
| `outcome_summary` | TEXT | Human-readable HIT/MISS description |
| `scored_at` | TEXT | Timestamp of scoring |
| `source_task_run_id` | TEXT | FK into `task_runs` (set for auto-emitted calls) |
| `confounded` | INTEGER | `1` if the outcome was changed by an intervention (migration 010). Confounded calls are excluded from `fetch_scored_calls` / the track record, so a successful mitigation never scores as a miss (K2). Default `0`; nothing sets it until the decision-capture treatment-detector lands. |

Index: `(property_id, call_type, status, mature_by)`.

### `monthly_actuals` (migration `008_monthly_actuals.sql`)

The T12-reversion call type draws on a separate table that holds the full monthly P&L history for each property, independent of the budget-comparison GL feed. This table is the sole data source for T12 means and the T12 freshness gate.

| Column | Type | Description |
|---|---|---|
| `id` | TEXT PK | UUID |
| `property_id` | TEXT | FK into `properties` |
| `period` | TEXT | YYYY-MM |
| `account_code` | TEXT | GL account code |
| `account_name` | TEXT | GL account name (may be blank) |
| `amount` | REAL | Monthly net amount for the account |
| `source_file` | TEXT | Path of the CSV that was ingested |

Index: `(property_id, account_code, period)`.

### Lifecycle

```
open  ──[mature_by period closes + actuals available]──►  scored
```

- A call is created with `status = open`.
- `boxscore calls score --period <YYYY-MM>` fetches all open calls whose `mature_by` matches the period and runs the domain scorer.
- Each scorer is **freshness-gated**: if the required actuals have not yet landed, the scorer returns `Ok(None)` and the call stays `open` until the next invocation.
- On a hit or miss the call flips to `scored` and a `track_record` memory is upserted (see Recall & Self-Improvement below).

## Call Types

### `noi_diagnosis`

**What it predicts:** that a top negative NOI variance driver will move in a specific direction in the following period (normalize, persist, or worsen).

**Auto-emitted by:** `emit_noi_diagnosis_calls`, called from `analyze_variance` at the end of every variance run. Up to 5 calls emitted per period (one per top negative driver). Idempotent — re-running variance for a period that already has `noi_diagnosis` calls is a no-op.

**Payload shape:**

```json
{
  "account_code": "5120",
  "account_name": "Repairs & Maintenance",
  "category": "Repairs & Maintenance",
  "baseline_actual": 18000.0,
  "expected_direction": "normalize"
}
```

`expected_direction` is one of:
- `"normalize"` — magnitude will decrease next period (default until track record says otherwise)
- `"persist"` — magnitude will stay within ±10% of baseline
- `"worsen"` — magnitude will increase

**Category-aware direction (self-learning):** The emitter queries `db::category_normalize_rate` for each account's NOI category. If past `normalize` calls for that category scored below 0.5 accuracy with at least 10 scored samples (`NOI_DIRECTION_MIN_HISTORY = 10`), the emitter flips the direction to `"persist"`. This prevents the harness from repeatedly predicting reversion for structurally sticky costs (e.g. Taxes, Insurance) after the track record shows they do not revert.

**Confidence:** Fixed at `0.40` for auto-emitted calls.

**Scorer (`NoiDiagnosisScorer`):** Compares the absolute magnitude of `baseline_actual` against the `mature_by` period's GL actual for the same account code. Sign-convention agnostic.
- `"normalize"` hit: `|next_actual| < |baseline_actual|`
- `"worsen"` hit: `|next_actual| > |baseline_actual|`
- `"persist"` hit: `||next_actual| - |baseline_actual|| ≤ 10% × max(|baseline_actual|, 1)`
- Score is `1.0` (hit) or `0.0` (miss).

**Freshness gate:** `period_has_actuals(property_id, mature_by)` — a GL actuals row must exist for the mature_by period.

**Data source:** `gl_actuals` (budget-comparison feed, ingested via `ingest-standardized gl`).

---

### `delinquency_risk`

**What it predicts:** that a specific resident will become or remain delinquent within the mature_by period.

**Emitted via:** `calls import --call-type delinquency_risk` — imported from a BDDRE (Boxscore Delinquency & Default Risk Engine) output CSV. Never auto-emitted by the variance engine.

**Payload shape:**

```json
{
  "resident_code": "T-00412",
  "probability": 0.72,
  "window_days": 45
}
```

`probability` (0–1) is sourced from the CSV's `predictive_risk_score` column (divided by 100). `window_days` is context for the operator and does not affect scoring. `probability` is optional; if absent the call is scored directionally.

**CSV columns consumed:** `resident_code`, `predictive_risk_score`.

**Scorer (`DelinquencyRiskScorer`):**
- With `probability` → Brier-style score: `1 - (probability - actual)²` where `actual` is 1.0 if the resident owed > $0.
- Without `probability` → directional: `1.0` if delinquent, `0.0` if current.

**Freshness gate:** `receivables_snapshot_exists(property_id, mature_by)`.

**Data source:** Per-resident `unit_receivables` rows keyed to `mature_by` period, populated during standardized ingestion.

---

### `renewal_rec`

**What it predicts:** the recommended renewal rent for a unit/resident pair, scored against the actual rent in the mature_by period.

**Emitted via:** `calls import --call-type renewal_rec` — imported from an RPCOE (Renewal Pricing & Collections Opportunity Engine) output CSV.

**Payload shape:**

```json
{
  "unit_label": "C-0215",
  "resident_code": "T-00412",
  "recommended_rent": 1425.00
}
```

**CSV columns consumed:** `unit`, `resident_code`, `recommended_new_rent`, `confidence` (optional; `"high"` → 0.9, `"medium"` → 0.6, `"low"` → 0.4; defaults to 0.6).

**Scorer (`RenewalRecScorer`):** Looks up `unit_label` + `resident_code` in `unit_leases` for the mature_by period.
- Resident renewed: closeness score = `max(0, 1 - |actual_rent - recommended_rent| / recommended_rent)`. Score of 1.0 means the actual rent exactly matched the recommendation.
- Resident absent (did not renew): score `0.0`.

**Freshness gate:** `leases_snapshot_exists(property_id, mature_by)`.

**Data source:** Per-unit `unit_leases` rows keyed to `mature_by` period.

---

### `t12_reversion`

**What it predicts:** that a P&L account whose current-month actual deviates materially from its trailing-12-month mean will partially or fully revert toward that mean in the next month.

**Auto-emitted by:** `emit_t12_reversion_calls`, called from `calls backfill-t12` (batch) or triggered per-period. Idempotent per `(property, period, t12_reversion)`. Only emits for accounts with a mapped NOI category (non-`Unmapped`) — balance-sheet lines that appear in the Yardi budget-comparison universe are silently skipped.

**Emission thresholds:**
- Minimum history: 6 months (`T12_MIN_HISTORY`).
- Minimum deviation: the greater of `20% × |trailing mean|` or `$1,000` (`T12_DEV_PCT = 0.20`, `T12_DEV_ABS = 1000.0`).
- Top 8 deviations by absolute size emitted per period (`T12_TOP_N = 8`).
- `mature_by` = origin_period + 1 month. Confidence fixed at `0.50`.

**Payload shape:**

```json
{
  "account_code": "5220",
  "account_name": "Contract Services",
  "category": "Repairs & Maintenance",
  "baseline_mean": 4800.0,
  "actual_at_origin": 11200.0
}
```

`baseline_mean` is the trailing-12-month mean of monthly actuals up to and including the origin period. `actual_at_origin` is the origin month's actual.

**Scorer (`T12ReversionScorer`):** Graded mean-reversion score — the fraction of the original deviation from the trailing mean that closed in the mature_by month.

```
dev0 = actual_at_origin - baseline_mean
dev1 = next_actual     - baseline_mean
score = clamp((|dev0| - |dev1|) / |dev0|, 0.0, 1.0)
```

Score `1.0` means full reversion (or overshoot past the mean); `0.0` means no reversion at all. The score is continuous, not binary.

**Freshness gate:** `monthly_actuals_exist(property_id, mature_by)` — a row in the `monthly_actuals` table must exist for the mature_by period.

**Data source:** `monthly_actuals` table, populated from `gl_monthly_actuals.csv` via `ingest monthly-actuals`. This is budget-free — no budget data is required.

---

## Commands

### Record a call manually

Skills emit calls via:

```bash
boxscore calls record \
  --property "maplewood" \
  --call-type delinquency_risk \
  --origin-period 2026-05 \
  --mature-by 2026-06 \
  --confidence 0.72 \
  --payload '{"resident_code":"T-00412","probability":0.72,"window_days":45}'
```

Required flags: `--property`, `--call-type`, `--origin-period`, `--mature-by`, `--payload` (valid JSON object string). Optional: `--confidence` (0–1 float).

`noi_diagnosis` and `t12_reversion` calls are auto-emitted internally — no manual invocation needed for those.

### List all calls

```bash
boxscore calls list
```

Returns all calls as a JSON array.

### Score matured calls

```bash
boxscore calls score --period 2026-05
```

Scores every open call whose `mature_by` equals the given period. Freshness-gated per scorer — calls whose outcome data has not landed stay `open` and are retried on the next invocation. Writes a `track_record` memory for every property + call type that has at least one newly scored call. Returns `{ "period": "2026-05", "scored": <n> }`.

### View the track record

```bash
boxscore calls track-record
boxscore calls track-record --property "maplewood"
boxscore calls track-record --call-type noi_diagnosis
```

Both `--property` and `--call-type` are optional filters. Without flags, returns the 100 most recent track-record memories as JSON. Each entry shows batting average, number of scored calls, and calibration gap (`mean_confidence - batting_avg`; positive = overconfident).

### Bootstrap the `noi_diagnosis` track record from historical GL

```bash
boxscore calls backfill \
  --property "maplewood" \
  --through 2026-04 \
  --lookback 11
```

Walks `lookback + 1` periods back from `through` (chronological order), runs `analyze_variance` for each (skipping periods with no GL data), then scores all matured calls. Fully idempotent. Default `--lookback 11` gives 12 months of history. Returns `{ "periods_analyzed": N, "calls_scored": M }`.

### Bootstrap the `t12_reversion` track record from `monthly_actuals`

```bash
boxscore calls backfill-t12 \
  --property "maplewood" \
  --through 2026-04 \
  [--from 2025-01]
```

Emits `t12_reversion` calls for every period from `--from` through `--through` (inclusive), then scores matured ones. If `--from` is omitted, starts from the earliest `monthly_actuals` period + 12 months (ensuring a full trailing window for the first emission). Returns `{ "periods_emitted": N, "calls_scored": M }`.

### Import per-unit calls from an engine CSV

```bash
boxscore calls import \
  --call-type delinquency_risk \
  --property "maplewood" \
  --period 2026-05 \
  --file /path/to/bddre_output.csv

boxscore calls import \
  --call-type renewal_rec \
  --property "maplewood" \
  --period 2026-05 \
  --file /path/to/rpcoe_output.csv
```

Imports `delinquency_risk` or `renewal_rec` calls from an engine CSV. Idempotent per `(property, period, call_type)` — if calls already exist for that combination, the import is a no-op. Returns `{ "imported": N, "call_type": "..." }`.

### Reversion report (calls by NOI category)

```bash
boxscore calls reversion-report --call-type t12_reversion
boxscore calls reversion-report --call-type noi_diagnosis --property "maplewood"
```

Aggregates scored calls by NOI category (derived from `gl_actuals.category` via the account mapping). For each category, prints `n`, `mean_score`, and `pct_strong` (fraction of calls with score > 0.5). Results are sorted by sample count descending. `--property` is optional; omit to aggregate across all properties. Accounts with no known category bucket as `"Unmapped"`.

### Ingest monthly actuals (deep history)

```bash
boxscore ingest monthly-actuals \
  --property "maplewood" \
  --file data/example/maplewood/Standardized/gl_monthly_actuals.csv
```

Ingests a `gl_monthly_actuals.csv` into the `monthly_actuals` table. No task_run overhead. Returns `{ "ingested": N }`. This is the feed for T12-reversion calls and the `t12` subcommand.

---

## Recall & Self-Improvement

### Track-record memories in the agent prompt

Every time a call is scored, `score_due_calls` upserts a `track_record` memory for that property + call type via `db::upsert_memory`. The memory stores the headline string: `"<call_type>: <batting_avg>% (<n> scored, calibration <±gap>)"`.

`ask.rs` injects the 10 most recent track-record memories into the system prompt:

```
Harness track record (your past calls, scored against actuals):
- noi_diagnosis: 67% (6 scored, calibration +0.13)
- t12_reversion: 58% (24 scored, calibration -0.02)
- delinquency_risk: 80% (5 scored, calibration -0.05)
Weight your confidence by this record; if a call type has a poor batting average, hedge accordingly.
```

The calibration gap is `mean_confidence - batting_avg`. Positive means overconfident; negative means underconfident.

### Self-calibrating variance confidence

`variance.rs::calibrate_confidence` adjusts the base confidence score reported in the variance analysis by the harness's `noi_diagnosis` batting average:

```
adjusted = base + (batting_avg - 0.5) × 0.2   (clamped 0–0.95)
```

No-op until at least 3 `noi_diagnosis` calls are scored. A perfect record (1.0) nudges up by +0.10; a zero-percent record nudges down by -0.10.

### Category-aware NOI direction

`emit_noi_diagnosis_calls` calls `db::category_normalize_rate` for each account's category before emitting. If the category has ≥ 10 scored `normalize` calls with a hit rate below 50%, the new call's `expected_direction` is set to `"persist"` instead of `"normalize"`. This means structurally sticky cost categories (Taxes, Insurance) stop generating false reversion predictions once the record shows they don't revert.

---

## Deep-History & Category Pipeline

Running T12-reversion analysis on a new property requires two one-time setup pipelines.

### 1. Build the monthly actuals history

The GL parquet contains the full transaction history but does not live in Boxscore's database. The ETL script rolls it up to monthly account totals:

```bash
# From repo root:
python3 your-etl/gl_monthly_from_parquet.py \
  --property-dir data/example/maplewood \
  --property-id "Maplewood Commons"
```

This reads `Standardized/gl_transactions.parquet` + `Standardized/budget_comparison.csv` (for the P&L account universe), groups by `(period, account_code)`, and writes `Standardized/gl_monthly_actuals.csv`. Balance-sheet codes are excluded — only accounts present in `budget_comparison.csv` are included.

Then ingest into Boxscore:

```bash
boxscore ingest monthly-actuals \
  --property "maplewood" \
  --file data/example/maplewood/Standardized/gl_monthly_actuals.csv
```

Then build the T12 track record:

```bash
boxscore calls backfill-t12 --property "maplewood" --through 2026-04
```

### 2. Derive and import account categories

T12-reversion calls require that each account have a mapped NOI category — the emitter skips any account where `category_for_account` returns `None` or `"Unmapped"`. The deriver reads the property's `chart_of_accounts.csv`, walks the sub-total hierarchy, and produces an `account_mappings_derived.csv` file with a `suggested_category` for each detail account:

```bash
python3 your-etl/derive_account_categories.py \
  --property-dir data/example/maplewood
# or for all three properties:
python3 your-etl/derive_account_categories.py --all
```

Output: `Standardized/account_mappings_derived.csv` (columns: `source_system`, `property_scope`, `account_code`, `account_name`, `current_category`, `suggested_category`, `confidence_score`, `status`, `reviewed_category`, `review_notes`).

Import into Boxscore:

```bash
boxscore accounts import-review \
  --file data/example/maplewood/Standardized/account_mappings_derived.csv
```

This reclassifies the `category` field on existing `gl_actuals` rows and writes account mappings. After import, run the reversion report to see which categories have the strongest (or weakest) reversion signal:

```bash
boxscore calls reversion-report --call-type t12_reversion --property "maplewood"
```

---

## Reading the Output

### `calls track-record`

Returns the most recent track-record memories per property + call type. Key fields:

- `batting_avg` — fraction of scored calls that hit (or mean graded score for `t12_reversion`).
- `n` — number of scored calls in the sample.
- `calibration_gap` — `mean_confidence - batting_avg`. Overconfident calls have a positive gap; underconfident ones negative.

A `noi_diagnosis` batting average below 50% on a category is the threshold that flips future calls for that category to `"persist"`.

### `calls reversion-report --call-type <t>`

Aggregates scored calls by NOI category. Output per category:

| Field | Meaning |
|---|---|
| `category` | NOI category label (e.g. `"Repairs & Maintenance"`) |
| `n` | Number of scored calls in the category |
| `mean_score` | Average score across all calls (0–1; graded for t12_reversion) |
| `pct_strong` | Fraction of calls with score > 0.5 |

High `mean_score` on `t12_reversion` means accounts in that category consistently revert toward their mean and are therefore predictable. Low `mean_score` means the category has structural drift (budget creep, seasonality, or step-change costs) — a signal to investigate rather than assume reversion.

---

## Operating Cadence

The call ledger slots into the monthly close run order:

```bash
# After ingest-standardized gl + close-readiness passes:
boxscore calls score --period <YYYY-MM>   # score last period's calls; refresh track record
boxscore calls track-record               # inspect batting averages before publishing

# For per-unit engine outputs (when available):
boxscore calls import --call-type delinquency_risk --property <P> --period <YYYY-MM> --file bddre.csv
boxscore calls import --call-type renewal_rec      --property <P> --period <YYYY-MM> --file rpcoe.csv

# After refreshing monthly_actuals (run at each close):
boxscore ingest monthly-actuals --property <P> --file Standardized/gl_monthly_actuals.csv
boxscore calls backfill-t12 --property <P> --through <YYYY-MM>
```

See `CLOSE_READINESS.md` for the full gate sequence. The `calls score` step sits immediately after close-readiness passes — GL actuals must be confirmed present before calls are scored.
