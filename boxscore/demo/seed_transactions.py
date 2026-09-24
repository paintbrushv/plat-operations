#!/usr/bin/env python3
"""Populate gl_transactions for the demo so the Ledger drill-down screen is rich
and reconciles to the monthly GL. Splits each (property, period, account) monthly
amount into a few believable postings with realistic payees.

Usage: seed_transactions.py <demo.db> <gl_actuals.csv>
"""
import csv
import random
import sqlite3
import sys
import uuid
from datetime import datetime, timezone

db_path, gl_csv = sys.argv[1], sys.argv[2]
RNG = random.Random(4242)

PAYEES = {
    "Payroll": ["ADP Payroll Services", "Onsite Team Payroll"],
    "Repairs & Maintenance": ["HD Supply", "Lone Star Turns LLC", "ProTurn Services", "Roto-Rooter"],
    "Utilities": ["Republic Services", "Duke Energy", "City Water Dept", "TXU Energy"],
    "Marketing": ["Apartments.com", "Google Ads", "ILS Network", "Resident Referral"],
    "Administrative": ["Yardi Systems", "Office Depot", "Smith & Howard CPA"],
    "Taxes": ["County Tax Assessor"],
    "Insurance": ["Marsh McLennan", "Assurant Specialty"],
    "Management Fees": ["Portfolio Management Fee"],
    "Rental Income": ["Resident Receipts"],
    "Concessions": ["Concession Credits"],
    "Bad Debt": ["Write-off / Collections"],
    "Other Income": ["Resident Ancillary", "Ancillary Receipts"],
}
RESIDENT_CATS = {"Rental Income", "Concessions", "Bad Debt", "Other Income"}
EOM = {1:31,2:28,3:31,4:30,5:31,6:30,7:31,8:31,9:30,10:31,11:30,12:31}


def split(amount, n):
    """Split amount into n parts summing exactly to amount (cents-safe)."""
    if n <= 1:
        return [round(amount, 2)]
    cents = round(amount * 100)
    parts = []
    for i in range(n - 1):
        # jittered share of the remainder
        share = cents // (n - i)
        jitter = int(share * RNG.uniform(-0.25, 0.25))
        p = max(min(share + jitter, cents), cents - share * (n - i - 1))
        parts.append(p)
        cents -= p
    parts.append(cents)
    return [round(p / 100, 2) for p in parts]


con = sqlite3.connect(db_path)
cur = con.cursor()
pids = {name: pid for pid, name in cur.execute("SELECT id, name FROM properties")}
ecode = {name: f"RTW{ i }" for i, (_, name) in enumerate(
    cur.execute("SELECT id, name FROM properties"), start=1)}
now = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")

rows, src_row = [], 1
with open(gl_csv) as f:
    for r in csv.DictReader(f):
        prop, period, code, name, cat, amount = (
            r["property"], r["period"], r["account_code"], r["account_name"],
            r["category"], float(r["amount"]))
        pid = pids.get(prop)
        if pid is None or abs(amount) < 0.005:
            continue
        yy, mm = int(period[:4]), int(period[5:7])
        is_res = 1 if cat in RESIDENT_CATS else 0
        paylist = PAYEES.get(cat, ["Vendor"])
        # resident revenue posts as many small receipts; vendors as 1-3 invoices
        n = 1 if code == "4399" else (RNG.randint(2, 4) if is_res else RNG.randint(1, 3))
        for amt in split(amount, n):
            day = RNG.randint(1, EOM[mm])
            payee = ("Resident #%d" % RNG.randint(1000, 9999)) if is_res and code != "4399" \
                else RNG.choice(paylist)
            remarks = "engagement bonus 👋" if code == "4399" else ""
            rows.append((str(uuid.uuid4()), pid, ecode[prop], code,
                         f"{yy:04d}-{mm:02d}-{day:02d}", period, payee, is_res,
                         None, f"INV-{RNG.randint(10000,99999)}", amt, remarks,
                         "demo/transactions", src_row, now))
            src_row += 1

cur.executemany(
    """INSERT INTO gl_transactions
       (id, property_id, entity_code, account_code, txn_date, period, payee,
        is_resident, control, reference, amount, remarks, source_file, source_row, created_at)
       VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)""", rows)
con.commit()
con.close()
print(f"  Inserted {len(rows)} gl_transactions (Ledger drill-down)")
