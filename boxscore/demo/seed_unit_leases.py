#!/usr/bin/env python3
"""Insert synthetic per-unit unit_leases into the demo DB.

The legacy `boxscore ingest` command has no `unit-leases` kind, so we write the
rows directly — matching migration 007's schema — after the properties have been
ingested (so property_id lookups resolve). This populates the F3 Renewals /
Lease-Expiration screen with a live-rent-roll-like spread of market vs charge
rent (and an underpriced tail).

Usage: seed_unit_leases.py <demo.db path> <unit_leases.csv path>
"""
import csv
import sqlite3
import sys
import uuid
from datetime import datetime, timezone

db_path, csv_path = sys.argv[1], sys.argv[2]
con = sqlite3.connect(db_path)
cur = con.cursor()

pids = {name: pid for pid, name in cur.execute("SELECT id, name FROM properties")}
now = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def _num(v):
    return float(v) if v not in ("", None) else None


inserted = 0
with open(csv_path) as f:
    for r in csv.DictReader(f):
        pid = pids.get(r["property"])
        if not pid:
            print(f"  WARN: no property '{r['property']}' — skipping")
            continue
        cur.execute(
            """INSERT INTO unit_leases
               (id, property_id, as_of_date, unit_label, resident_code,
                resident_name, market_rent, charge_rent,
                source_file, source_row, created_at)
               VALUES (?,?,?,?,?,?,?,?,?,?,?)""",
            (str(uuid.uuid4()), pid, r["as_of_date"], r["unit_label"],
             r["resident_code"] or None, r["resident_name"] or None,
             _num(r["market_rent"]), _num(r["charge_rent"]),
             "demo/unit_leases.csv", inserted + 2, now),
        )
        inserted += 1

con.commit()
con.close()
print(f"  Inserted {inserted} unit_leases")
