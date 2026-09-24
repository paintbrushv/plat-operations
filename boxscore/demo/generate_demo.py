#!/usr/bin/env python3
"""Generate a believable *synthetic* institutional multifamily portfolio for the
Boxscore demo harness — no live data, cheeky asset names, realistic GL/income.

Outputs CSVs (in ./data) in the exact shapes the legacy `boxscore ingest`
command consumes, so they load through the real, schema-correct ingestion path
into an isolated demo DB (see seed.sh). Categories use the 12 canonical Boxscore
NOI categories so Revenue/Expense classification + the NOI bridge work.

Numbers are tuned to land in institutional sanity bands: NOI margin ~52-58% of
EGI, per-unit NOI ~$8k-13k/yr, implied cap rate ~5.0-5.8% (printed + flagged).
Deterministic (seeded) so re-runs are reproducible.
"""
from __future__ import annotations
import csv
import random
from pathlib import Path

OUT = Path(__file__).parent / "data"
OUT.mkdir(parents=True, exist_ok=True)

RNG = random.Random(80808)  # deterministic

# 13 month-ends: 2025-05 .. 2026-05 (gives a full T12 ending at the latest month)
PERIODS = []
y, m = 2025, 5
for _ in range(13):
    PERIODS.append((y, m))
    m += 1
    if m > 12:
        m, y = 1, y + 1
LABELS = [f"{yy:04d}-{mm:02d}" for yy, mm in PERIODS]
# month-end dates (28/30/31) for snapshots
_EOM = {1: 31, 2: 28, 3: 31, 4: 30, 5: 31, 6: 30, 7: 31, 8: 31, 9: 30, 10: 31, 11: 30, 12: 31}
ASOF = [f"{yy:04d}-{mm:02d}-{_EOM[mm]:02d}" for yy, mm in PERIODS]

# ── Cheeky-but-institutional portfolio ───────────────────────────────────────
# Owner entity carries a wink for sharp-eyed viewers.
OWNER = "Bagholder Capital Partners, LP"
PROPS = [
    dict(name="Vantage at Yieldmore",        market="Phoenix, AZ",   units=312, rent=1585,
         pm="Apex Residential",        cls="A", vac=0.050, opex=0.430, tax=0.110, ins=0.040),
    dict(name="The Reserve at Cap Rate Cove", market="Tampa, FL",     units=248, rent=1520,
         pm="Greyskull Living",        cls="B+", vac=0.055, opex=0.445, tax=0.105, ins=0.052),
    dict(name="Promote Pointe",               market="Charlotte, NC", units=396, rent=1925,
         pm="Pinnacle Property Co.",   cls="A", vac=0.048, opex=0.420, tax=0.108, ins=0.038),
    dict(name="Alta Carry",                   market="Dallas, TX",    units=184, rent=1295,
         pm="Carryover Residential",   cls="B", vac=0.062, opex=0.470, tax=0.125, ins=0.045),
]

# Seasonal factors by calendar month (Sunbelt): vacancy worse in winter,
# utilities + turnover heavier in summer.
VAC_SEASON = {1:1.20,2:1.18,3:1.05,4:0.96,5:0.92,6:0.88,7:0.88,8:0.92,9:0.98,10:1.02,11:1.08,12:1.16}
UTIL_SEASON = {1:0.95,2:0.92,3:0.95,4:1.00,5:1.06,6:1.18,7:1.24,8:1.24,9:1.12,10:1.00,11:0.94,12:0.95}
RM_SEASON  = {1:0.92,2:0.90,3:0.98,4:1.05,5:1.12,6:1.18,7:1.20,8:1.16,9:1.06,10:1.00,11:0.92,12:0.90}

# Account template: (code, name, category, base, lo, hi)
#   base 'gpr'  -> share of gross potential rent (revenue contra/other)
#   base 'egi'  -> share of effective gross income (expenses, positive)
REV = [
    ("4000", "Gross Potential Rent",        "Rental Income", "gpr_full", 1.0,   1.0),
    ("4010", "Loss to Lease",               "Rental Income", "gpr_neg",  0.012, 0.022),
    ("4020", "Vacancy Loss",                "Rental Income", "vacancy",  0.0,   0.0),
    ("4030", "Model & Employee Units",      "Rental Income", "gpr_neg",  0.003, 0.005),
    ("4100", "Concessions",                 "Concessions",   "gpr_neg",  0.015, 0.028),
    ("4200", "Bad Debt",                    "Bad Debt",      "gpr_neg",  0.006, 0.014),
    ("4300", "RUBS / Utility Reimbursement","Other Income",  "gpr_pos",  0.050, 0.066),
    ("4310", "Application & Admin Fees",    "Other Income",  "gpr_pos",  0.004, 0.008),
    ("4320", "Pet Rent & Fees",             "Other Income",  "gpr_pos",  0.006, 0.011),
    ("4330", "Parking & Storage",           "Other Income",  "gpr_pos",  0.005, 0.013),
    ("4340", "Late Fees & Other Income",    "Other Income",  "gpr_pos",  0.003, 0.006),
]
EXP = [
    ("5000", "Onsite Payroll",              "Payroll",               "egi", 0.080, 0.092),
    ("5010", "Payroll Taxes & Benefits",    "Payroll",               "egi", 0.024, 0.030),
    ("5100", "Turnover & Make-Ready",       "Repairs & Maintenance", "egi", 0.020, 0.028),
    ("5110", "Contract Services",           "Repairs & Maintenance", "egi", 0.018, 0.026),
    ("5120", "R&M Supplies",                "Repairs & Maintenance", "egi", 0.014, 0.022),
    ("5200", "Electricity",                 "Utilities",             "egi", 0.014, 0.022),
    ("5210", "Water & Sewer",               "Utilities",             "egi", 0.024, 0.034),
    ("5220", "Trash & Recycling",           "Utilities",             "egi", 0.006, 0.010),
    ("5300", "Advertising & Marketing",     "Marketing",             "egi", 0.010, 0.018),
    ("5310", "Locator & Referral Fees",     "Marketing",             "egi", 0.004, 0.008),
    ("5400", "Office & Administrative",     "Administrative",        "egi", 0.014, 0.024),
    ("5410", "Legal & Professional",        "Administrative",        "egi", 0.004, 0.009),
    ("5500", "Real Estate Taxes",           "Taxes",                 "tax", 0.0,   0.0),
    ("5600", "Property Insurance",          "Insurance",             "ins", 0.0,   0.0),
    ("5700", "Management Fee",              "Management Fees",       "egi", 0.030, 0.030),
]

# Variance tendencies (actual vs budget) by category — believable drivers.
VAR_TENDENCY = {
    "Insurance": 1.075, "Taxes": 1.004, "Repairs & Maintenance": 1.045,
    "Marketing": 0.930, "Payroll": 1.010, "Utilities": 1.030,
    "Concessions": 1.060, "Bad Debt": 1.080, "Other Income": 1.020,
    "Rental Income": 1.000, "Administrative": 1.015, "Management Fees": 1.000,
}


def _pct(lo, hi):
    return lo + (hi - lo) * RNG.random()


# First + last names for synthetic residents (no real PII — generic pools).
FIRST_NAMES = [
    "Avery", "Bianca", "Carlos", "Diana", "Elijah", "Farrah", "Gabriel", "Harper",
    "Ivan", "Jolene", "Kareem", "Lucia", "Marcus", "Nadia", "Omar", "Priya",
    "Quincy", "Rosa", "Sterling", "Tamara", "Ulysses", "Vera", "Wesley", "Ximena",
    "Yusuf", "Zaria",
]
LAST_NAMES = [
    "Alvarez", "Brooks", "Chen", "Delgado", "Emerson", "Fontaine", "Gupta", "Hassan",
    "Ito", "Jennings", "Kowalski", "Lindqvist", "Mbeki", "Nakamura", "Okonkwo",
    "Petrov", "Quintero", "Reyes", "Solberg", "Thompson", "Underwood", "Vasquez",
    "Whitfield", "Xiong", "Yamada", "Zimmerman",
]


def _resident_name(rng):
    return f"{rng.choice(FIRST_NAMES)} {rng.choice(LAST_NAMES)}"


def build_unit_receivables():
    """Synthetic per-resident aged receivables for the LATEST period only.

    Produces a realistic delinquent tail (~6-9% of units owing), with a worst
    resident around 2-3x monthly rent and a spread of aging severities, plus a
    handful of prepaid residents (negative current_owed) so the delinquency
    screen shows POSITIVE green. Deterministic.
    """
    rng = random.Random(909090)  # independent deterministic stream
    asof = ASOF[-1]  # latest period (2026-05-31)
    rows = []
    for p in PROPS:
        units, rent = p["units"], p["rent"]
        used_codes = set()

        def _code():
            while True:
                c = f"t{rng.randint(100000, 999999):06d}"
                if c not in used_codes:
                    used_codes.add(c)
                    return c

        # delinquent residents: ~6-9% of units
        n_delq = max(3, round(units * rng.uniform(0.06, 0.09)))
        for i in range(n_delq):
            # severity bucket: most are light, a tail is heavy
            roll = rng.random()
            if roll < 0.45:        # current-but-owed / light (partial month)
                amt = rent * rng.uniform(0.15, 0.60)
            elif roll < 0.72:      # one month behind
                amt = rent * rng.uniform(0.80, 1.25)
            elif roll < 0.90:      # two months behind
                amt = rent * rng.uniform(1.40, 1.90)
            else:                  # severe — worst residents (2-3x rent)
                amt = rent * rng.uniform(2.10, 3.00)
            total_delinquent = round(amt, 2)
            # days_late aligned with the same severity roll so aging buckets
            # match the dollar tiers (current / 0-30 / 31-60 / 61-90 / 90+).
            if roll < 0.45:
                days_late = rng.randint(0, 28)        # current-but-owed / light
            elif roll < 0.72:
                days_late = rng.randint(31, 58)       # one month behind
            elif roll < 0.90:
                days_late = rng.randint(62, 88)       # two months behind
            else:
                days_late = rng.randint(95, 160)      # severe / 90+
            # a partial-paying resident may carry a positive current_owed too
            current_owed = round(total_delinquent * rng.uniform(0.25, 0.60), 2) \
                if roll < 0.45 else round(total_delinquent, 2)
            rows.append((p["name"], asof, _code(), _resident_name(rng),
                         "current", total_delinquent, current_owed, days_late))

        # prepaid residents: a few credits so POSITIVE green renders
        n_prepaid = max(2, round(units * rng.uniform(0.010, 0.018)))
        for i in range(n_prepaid):
            prepay = round(rent * rng.uniform(0.20, 1.10), 2)
            rows.append((p["name"], asof, _code(), _resident_name(rng),
                         "prepaid", 0.0, -prepay, ""))

    return rows


def build_unit_leases():
    """Synthetic per-unit leases (latest period) for the renewals screen.

    One row per occupied unit, capped per property for demo speed. charge_rent is
    spread around market_rent: ~20% underpriced (<95% market -> drives the F3
    "underpriced" green-opportunity flag), ~15% over-market, the rest near market.
    Deterministic.
    """
    rng = random.Random(717171)  # independent deterministic stream
    asof = ASOF[-1]
    cap = 60  # cap rows/property for demo render speed
    rows = []
    for p in PROPS:
        units, rent = p["units"], p["rent"]
        occ = round(units * (1.0 - p["vac"]))
        n = min(cap, occ)
        # building/unit numbering: B## - U###
        for i in range(n):
            bldg = 1 + (i // 12)
            unit_num = 100 + (i % 12)
            unit_label = f"{bldg:02d}-{unit_num:03d}"
            # market rent: small per-unit variation around the property base
            market = round(rent * rng.uniform(0.94, 1.10))
            roll = rng.random()
            if roll < 0.20:        # underpriced (<95% market)
                charge = round(market * rng.uniform(0.84, 0.945))
            elif roll < 0.35:      # over-market (loss-to-lease unwind candidate)
                charge = round(market * rng.uniform(1.02, 1.08))
            else:                  # near market
                charge = round(market * rng.uniform(0.96, 1.015))
            code = f"t{rng.randint(100000, 999999):06d}"
            rows.append((p["name"], asof, unit_label, code, _resident_name(rng),
                         market, charge))
    return rows


def build():
    gl_actuals, gl_budgets = [], []
    rent_rows, delq_rows, leasing_rows, coll_rows = [], [], [], []
    sanity = []

    for p in PROPS:
        units, rent = p["units"], p["rent"]
        # stable per-account share draws so the mix is consistent across months
        share = {code: _pct(lo, hi) for code, _, _, _, lo, hi in REV + EXP}

        annual_noi = 0.0
        for idx, (yy, mm) in enumerate(PERIODS):
            label = LABELS[idx]
            asof = ASOF[idx]
            growth = 1.0 + 0.0028 * idx          # ~0.28%/mo market rent growth
            gpr_full = units * rent * growth      # gross potential (monthly)
            vac_pct = p["vac"] * VAC_SEASON[mm]

            # ---- BUDGET pass (smooth, planned) then ACTUAL (budget * variance) ----
            def emit(code, name, cat, amount_budget, amount_actual):
                gl_budgets.append((p["name"], label, code, name, cat, round(amount_budget, 2)))
                gl_actuals.append((p["name"], label, code, name, cat, round(amount_actual, 2)))

            # revenue
            rev_actual_total = 0.0
            for code, name, cat, base, lo, hi in REV:
                if base == "gpr_full":
                    b = gpr_full
                elif base == "vacancy":
                    b = -gpr_full * p["vac"]                 # budgeted vacancy
                elif base == "gpr_neg":
                    b = -gpr_full * share[code]
                else:  # gpr_pos
                    b = gpr_full * share[code]
                # actual
                if base == "vacancy":
                    a = -gpr_full * vac_pct                  # seasonal actual vacancy
                else:
                    tend = VAR_TENDENCY.get(cat, 1.0)
                    a = b * tend * (1.0 + RNG.uniform(-0.015, 0.015))
                emit(code, name, cat, b, a)
                rev_actual_total += a

            egi_actual = rev_actual_total
            egi_budget = gpr_full * (1 - p["vac"]) + sum(
                (-gpr_full * share[c] if base == "gpr_neg" else gpr_full * share[c])
                for c, _, _, base, _, _ in REV if base in ("gpr_neg", "gpr_pos")
            ) + gpr_full  # approx; expenses scale off actual EGI below

            # expenses (positive); scale off actual EGI for realism
            exp_actual_total = 0.0
            for code, name, cat, base, lo, hi in EXP:
                if base == "tax":
                    frac = p["tax"]
                elif base == "ins":
                    frac = p["ins"]
                else:
                    frac = share[code]
                season = 1.0
                if cat == "Utilities":
                    season = UTIL_SEASON[mm]
                elif cat == "Repairs & Maintenance":
                    season = RM_SEASON[mm]
                # Well-run portfolio: trim controllable opex; keep taxes &
                # insurance at full (realistic Sunbelt pain points).
                trim = 0.93 if base == "egi" else 1.0
                b = egi_actual * frac * trim
                tend = VAR_TENDENCY.get(cat, 1.0)
                a = b * tend * season * (1.0 + RNG.uniform(-0.012, 0.012))
                emit(code, name, cat, b, a)
                exp_actual_total += a

            # ---- Easter egg: a tiny, real Other Income line. Noticeable to a
            # careful viewer reading account-level detail; invisible at a glance.
            if p["name"] == "Promote Pointe":
                egg = 73.0 + idx  # trivially small, grows a hair each month
                emit("4399", "Hi there, retwit \U0001F44B", "Other Income", egg, egg)
                rev_actual_total += egg
                egi_actual += egg

            noi = rev_actual_total - exp_actual_total
            annual_noi += noi if idx >= 1 else 0.0  # T12 ending latest = last 12

            # ---- operating snapshots (month-end) ----
            occ = 1.0 - vac_pct
            occupied = round(units * occ)
            vacant = units - occupied
            notice = round(units * 0.03)
            down = round(units * 0.008)
            leased = min(units, occupied + round(units * 0.02))
            market_rent_total = round(units * rent * growth)
            in_place_total = round(occupied * rent * growth * 0.985)
            rent_rows.append((p["name"], asof, occupied, vacant, leased, notice, down,
                              market_rent_total, in_place_total))

            delq_units = round(units * (0.018 + 0.01 * VAC_SEASON[mm] / 1.2))
            delq_amt = round(delq_units * rent * 0.55)
            prepaid = round(units * rent * 0.012)
            delq_rows.append((p["name"], asof, delq_amt, delq_units, prepaid))

            leads = round(units * (0.55 + 0.25 * (UTIL_SEASON[mm] - 0.95)))
            tours = round(leads * 0.42)
            apps = round(tours * 0.46)
            approvals = round(apps * 0.82)
            move_ins = round(approvals * 0.9)
            move_outs = round(move_ins * RNG.uniform(0.85, 1.1))
            conc_amt = round(gpr_full * share["4100"])
            leasing_rows.append((p["name"], asof, leads, tours, apps, approvals,
                                 move_ins, move_outs, conc_amt))

            high_risk = round(delq_units * 0.4)
            pricing_opp = round(market_rent_total - in_place_total) * 0.15
            missed_fee = round(units * 6.5)
            on_time = round(100 - (delq_units / units) * 100 * 1.4, 1)
            coll_rows.append((p["name"], asof, float(delq_amt), delq_units, high_risk,
                              round(delq_amt + pricing_opp, 2), round(pricing_opp, 2),
                              float(missed_fee), on_time))

        # sanity (annualize the latest 12 months ending at the final period)
        per_unit_noi = annual_noi / units
        egi_annual = sum(r[5] for r in gl_actuals
                         if r[0] == p["name"] and r[4] in
                         ("Rental Income", "Concessions", "Bad Debt", "Other Income")) * (12 / 13)
        margin = annual_noi / egi_annual if egi_annual else 0
        implied_value_per_unit = {"A": 215000, "B+": 190000, "B": 160000}[p["cls"]]
        cap = per_unit_noi / implied_value_per_unit
        flag = "" if (7000 <= per_unit_noi <= 13500 and 0.045 <= cap <= 0.065) else "  <-- CHECK"
        sanity.append((p["name"], units, round(per_unit_noi), round(margin * 100, 1),
                       round(cap * 100, 2), flag))

    # ── write CSVs ───────────────────────────────────────────────────────────
    with open(OUT / "properties.csv", "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["name", "market", "unit_count", "owner_entity", "property_manager"])
        for p in PROPS:
            w.writerow([p["name"], p["market"], p["units"], OWNER, p["pm"]])

    for fn, rows in [("gl_actuals.csv", gl_actuals), ("gl_budgets.csv", gl_budgets)]:
        with open(OUT / fn, "w", newline="") as f:
            w = csv.writer(f)
            w.writerow(["property", "period", "account_code", "account_name", "category", "amount"])
            w.writerows(rows)

    with open(OUT / "rent_roll.csv", "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["property", "as_of_date", "occupied_units", "vacant_units", "leased_units",
                    "notice_units", "down_units", "market_rent_total", "in_place_rent_total"])
        w.writerows(rent_rows)

    with open(OUT / "delinquency.csv", "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["property", "as_of_date", "delinquent_amount", "delinquent_units", "prepaid_amount"])
        w.writerows(delq_rows)

    with open(OUT / "leasing.csv", "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["property", "as_of_date", "leads", "tours", "applications", "approvals",
                    "move_ins", "move_outs", "concessions_amount"])
        w.writerows(leasing_rows)

    with open(OUT / "collections.csv", "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["property", "as_of_date", "total_delinquent", "delinquent_units", "high_risk_units",
                    "total_opportunity", "pricing_opportunity", "missed_fee_total", "avg_on_time_pct"])
        w.writerows(coll_rows)

    receivable_rows = build_unit_receivables()
    with open(OUT / "unit_receivables.csv", "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["property", "as_of_date", "resident_code", "resident_name",
                    "resident_status", "total_delinquent", "current_owed", "days_late"])
        w.writerows(receivable_rows)

    lease_rows = build_unit_leases()
    with open(OUT / "unit_leases.csv", "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["property", "as_of_date", "unit_label", "resident_code",
                    "resident_name", "market_rent", "charge_rent"])
        w.writerows(lease_rows)

    print(f"Wrote {len(receivable_rows)} unit_receivables rows (latest period).")
    print(f"Wrote {len(lease_rows)} unit_leases rows (latest period).")
    print(f"Wrote {len(PROPS)} properties, {len(gl_actuals)} GL actual rows, "
          f"{len(gl_budgets)} budget rows, {len(rent_rows)} snapshots/feed.")
    print(f"Periods: {LABELS[0]} .. {LABELS[-1]}  (latest = {LABELS[-1]})")
    print("\nSanity (T12 ending latest period):")
    print(f"  {'Asset':28} {'Units':>5} {'NOI/unit':>9} {'Margin%':>8} {'Cap%':>6}")
    for name, units, pun, margin, cap, flag in sanity:
        print(f"  {name:28} {units:>5} {pun:>9,} {margin:>8} {cap:>6}{flag}")


if __name__ == "__main__":
    build()
