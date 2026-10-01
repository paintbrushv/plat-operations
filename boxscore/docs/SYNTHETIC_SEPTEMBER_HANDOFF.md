# Synthetic September manager handoff in Boxscore

This is a technical rehearsal on a copy of Boxscore's demo database. It does
not use manager exports, the owner's correction, real property IDs, or a
signed accounting close. The only owner-supplied facts carried into the
scenario are the TC September 23 Yardi-to-ResMan direction and the CCAR
September 25 Yardi-to-Yardi direction. All row layouts, source IDs, financial
amounts, account mappings, budgets, and correction records are invented.

`boxscore synthetic-pms` accepts only property names starting `Synthetic `,
external IDs starting `SYN-`, and the two explicit layout versions
`yardi.synthetic-gl/1` and `resman.synthetic-gl/1`. It requires the boundary
file on every import. The database pins its exact property-period boundary,
including manager-specific source namespaces and account mappings. Changed
profiles, unknown columns/accounts, invalid dates or signs, conflicting
revisions, and revision gaps fail before acceptance. Each file's financial
writes occur in one SQLite transaction. Exact old or new revision retries do
not duplicate GL amounts; a correction appends a delta while retaining the
prior source revision. This is a **synthetic-only GL/P&L slice**. These layouts
must not be relabeled as real Yardi or ResMan adapters.

The synthetic close requires both feeds and rejects GL actuals for the period
that did not come from accepted adapter revisions.

The `pms_synthetic_closes` table freezes the September actual and budget NOI
at issue. Sealing now requires the task-run ID of a completed variance report
for the same property, period, and totals. Boxscore stores that report's body
in SQLite and creates a unique file that subsequent analyses do not replace.
A later correction changes the current view and produces a separate report,
while the issued report and close values remain unchanged. This snapshot is a
rehearsal artifact, not an owner-approved close.

| Property | Invented issued NOI | Invented restated NOI | Invented budget NOI |
|---|---:|---:|---:|
| TC | $55,000 | $54,500 | $57,000 |
| CCAR | $44,000 | $44,000 | $46,000 |

The TC change is one outgoing Yardi expense revision from $18,000 to $18,500,
accepted after the synthetic snapshot. A repeat correction and a later exact
replay of the original file change no GL amount. TC's two manager feeds also
reuse `TX-003` for separate equal $5,000 Other Income entries. CCAR's old and
new manager feeds both use the synthetic Yardi layout but have separate source
namespaces and account maps.

## Executable checks

Run `cargo test --test synthetic_pms_handoff` from `boxscore/`. The tests cover
both handoffs, budget/NOI totals, the frozen close, correction and retries,
changed boundary refusal, unknown layout/account/date/revision refusal,
transaction rollback, file-backed SQLite sealing, issued-report history,
stale or tampered report refusal, and rejection of a negative-expense budget
sign. It also rejects unattributed actuals before sealing.

A protected local run used a fresh SQLite backup of
`/home/ubuntu/projects/plat-operations/boxscore/data/boxscore_demo.db`. The
source SHA-256 remained
`aba1504252b4c0e0badc9a08c952b745536cf1d776a5721d739aae8df5936397`.
The original demo contained only March–May 2026 financial periods and three
properties. The working copy added two named synthetic September properties;
Boxscore's CLI imported the invented feeds and budget, sealed each snapshot,
ran variance analysis before and after TC's revision, then reopened a SQLite
backup. The current private run summary lives outside the repository at
`/tmp/boxscore-september-demo-20261001/v8-report/summary.json` and must not be
published with its database files. This run verified separate issued and
restated report paths, an unchanged issued report, and a matching restored
status.

## Still open

- September close-readiness marks both synthetic properties **not owner-ready**
  with data contracts `NOT_RUN`; operational feeds and genuine close approvals
  are absent. The synthetic seal deliberately does not grant owner readiness.
- Current host-owned grant checks remain unavailable. A report's stored
  history does not establish that a requester's current workspace/deal grants
  allow them to read it.
- The older demo database still contains negative expense rows from the prior
  public sample. The source sample has been corrected, and variance analysis
  now refuses net-negative expense totals, but the old database itself was not
  modified. See [report and sign controls](ISSUED_REPORTS_AND_SIGN_CONVENTION.md).
- Approved before/after Yardi and ResMan TBs, income statements, budgets,
  detailed GLs, subledgers, correction log, stable identity map, actual export
  layouts, and host-owned grants remain unavailable. Production adapters and
  real September row parity therefore remain blocked.
