# September 2026 manager handoff source intake

The real TC and CCAR adapters and row-parity tests are waiting for an approved
read-only source set. No September 2026 source export or correction record has
been accepted into this repository. Keep source files, credentials, and any
resident-level data outside Git and the public test fixtures.

| Property | Posting-date boundary | Outgoing feed | Incoming feed | Approved source location |
|---|---|---|---|---|
| TC | September 23, 2026 | Yardi through September 22 | ResMan from September 23 | Pending |
| CCAR | September 25, 2026 | Outgoing manager Yardi through September 24 | Incoming manager Yardi from September 25 | Pending |

For **each side of each boundary**, the minimum source package is the trial
balance, income statement, budget, detailed GL, export layout/version, stable
property and account identifiers, accounting basis, as-of timestamp, and
source revision or batch identifiers. Include any subledger needed to explain
GL totals and the handoff's opening/closing balance bridge. The actual
correction needs its source record ID, property, accounting and posting dates,
before/after values, authorizing record, and export containing the revision.
Its property and amount have not been supplied and must not be inferred from
the invented TC fixture.

An approval record must identify the read-only path or service, the owner who
approved it, permitted workspace and purpose, handling rules, and expiry.
Authorization evidence also needs the current host-owned workspace/deal grant
source and a scoped read-only test identity. No credentials belong in this
document or chat.

Once supplied, record source hashes and row counts in a protected local
manifest. Pin an adapter version to the actual export headers, source sign
convention, account mapping, property identity, and cutover authority. Prove
the following before treating the adapter as a real PMS connector:

1. Every accepted GL row traces to one approved source row; skipped and
   quarantined rows are counted with reasons. Duplicate record IDs across
   managers remain distinct, and exact retries have zero financial effect.
2. Account and category totals tie from each detailed GL to its trial balance
   and income statement, with explicit treatment of opening balances,
   accruals, contra revenue, expense credits, and unmapped accounts.
3. Budget and actual NOI match the approved September statements at both the
   pre-handoff and post-handoff snapshots. A correction retains the original
   issued report and produces a separate restatement with an exact delta.
4. Backup and restore preserve both report versions and all source revisions.
   Current grants are checked again after revocation and restore, including a
   denied cross-workspace/deal read.

Until that package and grant boundary exist, the synthetic handoff tests are
engineering evidence only. Real row parity, owner-approved close, host access,
and release readiness remain open.
