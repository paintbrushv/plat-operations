#!/usr/bin/env python3
"""Insert collection_snapshots into the demo DB.

The legacy `boxscore ingest` command has no `collections` kind, so we write the
collection_snapshots rows directly — matching migration 003's schema — after the
properties have been ingested (so property_id lookups resolve).

Usage: seed_collections.py <demo.db path> <collections.csv path>
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

inserted = 0
with open(csv_path) as f:
    for r in csv.DictReader(f):
        pid = pids.get(r["property"])
        if not pid:
            print(f"  WARN: no property '{r['property']}' — skipping")
            continue
        cur.execute(
            """INSERT INTO collection_snapshots
               (id, property_id, as_of_date, total_delinquent, delinquent_units,
                high_risk_units, total_opportunity, pricing_opportunity,
                missed_fee_total, avg_on_time_pct, source_file, source_row, created_at)
               VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)""",
            (str(uuid.uuid4()), pid, r["as_of_date"], float(r["total_delinquent"]),
             int(r["delinquent_units"]), int(r["high_risk_units"]),
             float(r["total_opportunity"]), float(r["pricing_opportunity"]),
             float(r["missed_fee_total"]), float(r["avg_on_time_pct"]),
             "demo/collections.csv", inserted + 2, now),
        )
        inserted += 1

con.commit()
con.close()
print(f"  Inserted {inserted} collection_snapshots")
