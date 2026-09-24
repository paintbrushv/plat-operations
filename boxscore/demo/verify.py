#!/usr/bin/env python3
"""Demo confidence check. Reads `boxscore close-readiness` JSON on stdin for the
close-readiness summary, then (if a demo.db path is passed as argv[1]) asserts
that every screen-critical table is populated, printing a ✓/✗ table.

Usage: boxscore close-readiness --period 2026-05 | verify.py [demo.db path]
"""
import json
import sqlite3
import sys

d = json.load(sys.stdin)
print(f"  Period {d['period']} — {d['summary']['property_count']} assets")
for p in d["properties"]:
    feeds = {f["name"]: f["status"] for f in p["feeds"]}
    req = ["Actual GL", "Budget GL", "Rent roll", "Delinquency", "Leasing", "Collections"]
    cells = " ".join(f"{n.split()[0][:4]}:{feeds.get(n,'?')[:4]}" for n in req)
    print(f"   {p['property'][:30]:30} {cells}")

# ── Every-screen-populated guarantee ─────────────────────────────────────────
if len(sys.argv) > 1:
    db_path = sys.argv[1]
    con = sqlite3.connect(db_path)
    cur = con.cursor()
    checks = [
        ("unit_receivables", "SELECT COUNT(*) FROM unit_receivables"),
        ("unit_leases", "SELECT COUNT(*) FROM unit_leases"),
        ("memories (track_record)",
         "SELECT COUNT(*) FROM memories WHERE memory_type='track_record'"),
        ("calls", "SELECT COUNT(*) FROM calls"),
    ]
    print("\n  Screen-data coverage:")
    all_ok = True
    for label, q in checks:
        try:
            n = cur.execute(q).fetchone()[0]
        except sqlite3.Error:
            n = 0
        mark = "✓" if n > 0 else "✗"
        if n == 0:
            all_ok = False
        print(f"   {mark} {label:26} {n:>6} rows")
    con.close()
    if not all_ok:
        print("  ✗ Some screens will be EMPTY in demo desk — re-run ./demo/seed.sh")
        sys.exit(1)
    print("  ✓ Every screen has data — demo desk is whole.")
