#!/usr/bin/env python3
"""Seed synthetic track-record memories + scored calls into the demo DB so the
Track Record screen shows a credible batting average instead of an empty state.

Inserts ~8 `memories` (memory_type='track_record') and ~12 `calls` (a mix of
`scored` with varied scores and a few `open`) referencing the demo property_ids.
Call types use values the engine knows: noi_diagnosis, collections_priority,
lease_expiration. Deterministic.

Usage: seed_track_record.py <demo.db path>
"""
import json
import random
import sqlite3
import sys
import uuid
from datetime import datetime, timezone

db_path = sys.argv[1]
RNG = random.Random(606060)
con = sqlite3.connect(db_path)
cur = con.cursor()

props = list(cur.execute("SELECT id, name FROM properties"))
pids = [pid for pid, _ in props]
now = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")

# ── Track-record memories (batting averages by call_type / category) ──────────
MEMORIES = [
    ("track_record", "global", "noi_diagnosis:Insurance",
     "Insurance over-budget calls: 4/5 normalized next period (80% hit rate)."),
    ("track_record", "global", "noi_diagnosis:Repairs & Maintenance",
     "R&M spike diagnoses: 5/7 reverted within one period (71% hit rate)."),
    ("track_record", "global", "noi_diagnosis:Marketing",
     "Marketing underspend flags: 3/4 persisted as planned (75% hit rate)."),
    ("track_record", "global", "collections_priority",
     "Collections priority calls: 9/12 residents cured or on plan (75%)."),
    ("track_record", "global", "lease_expiration",
     "Lease-expiration renewals: 6/8 closed at or above recommended rent (75%)."),
    ("track_record", "Promote Pointe", "noi_diagnosis:Utilities",
     "Utilities seasonal spikes called correctly 3/3 (100%) at Promote Pointe."),
    ("track_record", "Vantage at Yieldmore", "collections_priority",
     "Vantage delinquency triage: worst-first list cured 5/6 top accounts."),
    ("track_record", "global", "overall",
     "Overall scored-call hit rate: 27/36 (75%) across the portfolio."),
]

inserted_mem = 0
for mtype, scope, key, value in MEMORIES:
    cur.execute(
        """INSERT OR REPLACE INTO memories
           (id, memory_type, scope, key, value, confidence_score,
            source_task_run_id, created_at, updated_at)
           VALUES (?,?,?,?,?,?,?,?,?)""",
        (str(uuid.uuid4()), mtype, scope, key, value,
         round(RNG.uniform(0.65, 0.92), 2), None, now, now),
    )
    inserted_mem += 1

# ── Calls: a mix of scored (varied scores) + a few open ───────────────────────
CALL_TYPES = ["noi_diagnosis", "collections_priority", "lease_expiration"]
CATS = {
    "noi_diagnosis": ["Insurance", "Repairs & Maintenance", "Utilities", "Marketing"],
    "collections_priority": ["Bad Debt"],
    "lease_expiration": ["Rental Income"],
}
PERIODS = ["2026-01", "2026-02", "2026-03", "2026-04", "2026-05"]


def _summary(call_type, cat, hit):
    tag = "HIT" if hit else "MISS"
    if call_type == "noi_diagnosis":
        b = RNG.uniform(8000, 60000)
        n = b * (RNG.uniform(0.80, 0.97) if hit else RNG.uniform(1.03, 1.18))
        return f"{cat}: {b:.0f} -> {n:.0f} (normalize) {tag}"
    if call_type == "collections_priority":
        owed = RNG.uniform(900, 3200)
        return f"Top delinquent ${owed:.0f}: {'cured/on-plan' if hit else 'rolled later'} {tag}"
    return f"Renewal at/above recommended rent: {'closed' if hit else 'lost'} {tag}"


inserted_calls = 0
# 9 scored + 3 open = 12
for i in range(12):
    pid = RNG.choice(pids)
    ctype = CALL_TYPES[i % len(CALL_TYPES)]
    cat = RNG.choice(CATS[ctype])
    period = RNG.choice(PERIODS)
    made_at = f"{period}-01T09:00:00Z"
    mature_by = f"{period}-28T23:59:59Z"
    payload = json.dumps({"category": cat, "expected_direction": "normalize",
                          "period": period})
    if i < 9:  # scored
        score = round(RNG.uniform(0.30, 0.90), 2)
        hit = score >= 0.55
        cur.execute(
            """INSERT INTO calls
               (id, property_id, origin_period, call_type, status, made_at,
                mature_by, confidence, payload_json, outcome_json, score,
                outcome_summary, scored_at, source_task_run_id, created_at, updated_at)
               VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)""",
            (str(uuid.uuid4()), pid, period, ctype, "scored", made_at, mature_by,
             round(RNG.uniform(0.4, 0.8), 2), payload,
             json.dumps({"hit": hit}), score, _summary(ctype, cat, hit),
             mature_by, None, now, now),
        )
    else:  # open
        cur.execute(
            """INSERT INTO calls
               (id, property_id, origin_period, call_type, status, made_at,
                mature_by, confidence, payload_json, outcome_json, score,
                outcome_summary, scored_at, source_task_run_id, created_at, updated_at)
               VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)""",
            (str(uuid.uuid4()), pid, period, ctype, "open", made_at, mature_by,
             round(RNG.uniform(0.4, 0.8), 2), payload,
             None, None, None, None, None, now, now),
        )
    inserted_calls += 1

con.commit()
con.close()
print(f"  Inserted {inserted_mem} track_record memories, {inserted_calls} calls")
