#!/usr/bin/env python3
"""Insert synthetic per-resident unit_receivables into the demo DB.

The legacy `boxscore ingest` command has no `unit-receivables` kind, so we write
the rows directly — matching migration 006's schema — after the properties have
been ingested (so property_id lookups resolve). This populates the F2
Delinquency / Collections screen with a realistic aged-receivables tail.

Usage: seed_unit_receivables.py <demo.db path> <unit_receivables.csv path>
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
        current_owed = r.get("current_owed", "")
        days_late = r.get("days_late", "")
        cur.execute(
            """INSERT INTO unit_receivables
               (id, property_id, as_of_date, resident_code, resident_name,
                resident_status, total_delinquent, current_owed, days_late,
                source_file, source_row, created_at)
               VALUES (?,?,?,?,?,?,?,?,?,?,?,?)""",
            (str(uuid.uuid4()), pid, r["as_of_date"], r["resident_code"],
             r["resident_name"] or None, r["resident_status"] or None,
             float(r["total_delinquent"]),
             float(current_owed) if current_owed not in ("", None) else None,
             int(days_late) if days_late not in ("", None) else None,
             "demo/unit_receivables.csv", inserted + 2, now),
        )
        inserted += 1

con.commit()
con.close()
print(f"  Inserted {inserted} unit_receivables")
