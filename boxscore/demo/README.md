# Boxscore Demo Harness 🎥

A **fully synthetic** institutional multifamily portfolio for screen-sharing the
Boxscore ops harness publicly — **no live data ever touched**. Cheeky asset
names, believable GL / income / NOI, all four assets owner-ready green.

## Launch

```bash
./demo/seed.sh      # generate synthetic data + build the isolated demo DB (run once)
boxscore demo desk  # native command — launches the TUI against the demo DB
```

`boxscore demo desk` resolves `demo/demo.db` automatically (no env var). The
`./demo/run.sh` wrapper does the same and auto-seeds if the DB is missing. Inside the TUI: `1` Close Desk · `4` Statements
(T12) · arrow/`j``k` navigate · `[` `]` shift period · `:` ask · `q` quit.

## Isolation

Everything runs against `demo/demo.db` via the `DATABASE_URL` env var — the live
`boxscore.db` is never opened. To run any boxscore command against the demo:

```bash
DATABASE_URL="sqlite://$PWD/demo/demo.db" boxscore close-readiness --period 2026-05
DATABASE_URL="sqlite://$PWD/demo/demo.db" boxscore t12 --property "Promote Pointe"
```

## The portfolio (all fictional)

| Asset | Market | Units | NOI/unit | Margin | ~Cap |
|-------|--------|------:|---------:|-------:|-----:|
| Vantage at Yieldmore | Phoenix, AZ | 312 | ~$10.4k | 55.7% | 4.9% |
| The Reserve at Cap Rate Cove | Tampa, FL | 248 | ~$9.7k | 53.7% | 5.1% |
| Promote Pointe | Charlotte, NC | 396 | ~$12.8k | 55.5% | 6.0% |
| Alta Carry | Dallas, TX | 184 | ~$8.1k | 53.1% | 5.1% |

Owner of record: *Bagholder Capital Partners, LP* 😏. Data spans 2025-05 → 2026-05
(full T12), with seasonal occupancy, Sunbelt tax/insurance pressure, and
believable budget variances so the variance bridge has real drivers to surface.

## How it works

`generate_demo.py` writes CSVs to `data/` in the exact shapes the legacy
`boxscore ingest <kind> --file` command consumes, so they load through the **real
ingestion pipeline** (schema-correct, NOI categories valid). Four direct-insert
seeders fill the tables the legacy `ingest` command has no kind for, so **every
screen is populated** in `boxscore demo desk`:

| Seeder | Table(s) | Powers screen |
|--------|----------|---------------|
| `seed_collections.py` | `collection_snapshots` | Close Desk / Collections KPIs |
| `seed_unit_receivables.py` | `unit_receivables` | Delinquency / Collections (per-resident aged tail, incl. prepaids) |
| `seed_unit_leases.py` | `unit_leases` | Renewals / Lease-Expiration (market vs charge rent, underpriced tail) |
| `seed_track_record.py` | `memories`, `calls` | Track Record (batting averages + scored/open calls) |
| `seed_transactions.py` | `gl_transactions` | Ledger drill-down (reconciles to monthly GL) |

All seeders are deterministic; re-running `seed.sh` is idempotent. The synthetic
CSVs carry **no real PII** — resident names are drawn from generic first/last-name
pools and codes are random `t######` strings.

**Every-screen-populated guarantee.** `verify.py` runs at the end of `seed.sh`
and asserts row counts > 0 in `unit_receivables`, `unit_leases`, `memories`
(track_record), and `calls`, printing a ✓/✗ table and exiting non-zero if any
screen would be empty. So a green `seed.sh` means the demo is whole.

## Lane-CSV fallback for Delinquency / Renewals detail

The Delinquency (F2) and Renewals (F3) screens have two kinds of content:

1. **DB-driven tables/aggregates** — money, aging, and lease spreads. In demo
   these come from the synthetic `unit_receivables` / `unit_leases` rows seeded
   above, keyed to the four Bagholder properties.
2. **Lane-CSV detail panes** — the BDDRE risk watchlist
   (`bddre_intervention_queue.csv`, `bddre_pre_delinquency_watchlist.csv`) and the
   RPCOE renewal recommendations (`rpcoe_recommendations.csv`). These files are the
   **real** engine outputs and live only under the maplewood / juniper_fund / willow-brook
   lanes' `Reports/` dirs.

The four synthetic demo properties have **no lane CSVs of their own**. So in
`demo desk`, the F2/F3 detail panes must fall back to:

- **(a)** the seeded DB rows for the per-resident / per-unit table, and
- **(b)** the **union of all three real lanes'** watchlist / recommendation CSVs,
  labeled by lane, for the risk / recommendation pane.

This shows synthetic DB rows + real-lane CSV rows side by side — acceptable, and
it makes the demo look rich without faking per-property CSVs. **F2.6 / F3.6
implement this fallback.** (Outside demo, with real lanes, each property's own
`Reports/` CSVs drive its detail pane directly.)

> There's a small wink hidden in the GL for anyone reading closely. 👀
