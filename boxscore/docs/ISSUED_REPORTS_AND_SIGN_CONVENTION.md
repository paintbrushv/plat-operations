# Variance report history and demo expense signs

Each completed variance analysis now issues a new report file named with its
task-run ID. Boxscore creates that file once, stores its Markdown body and NOI
totals in `variance_report_artifacts`, and returns the unique path. A later
analysis of the same property and period cannot replace an earlier report.
The stored body remains available in the SQLite backup even if the generated
file is lost. The file is created with owner-only permissions on Unix.

For the synthetic September rehearsal, `synthetic-pms seal` requires the
`--report-task-run-id` from a completed variance analysis of the same property
and period. It checks that the file still matches the stored body and that the
report's actual and budget NOI match the current ledger before freezing the
synthetic close. A later correction produces a new report and a current view;
the issued report body, file, and close totals remain as they were. Older
synthetic close rows created before this rule have no report link and should
not be represented as fully evidenced closes.

This is a local report-history control. It does not authenticate the person
reading or issuing a report. A current host-owned workspace/deal grant source,
read-only test identity, and revocation test are still required before any
protected report or real September source is exposed through a host.

## Expense-sign reconciliation

Boxscore's NOI convention is revenue minus positive expense costs. Contra
revenue such as concessions and bad debt stays negative inside revenue. The
old public `data/sample` GL files put every expense in the negative direction:
40 actual rows and 40 budget rows across five property-periods. That made the
old demo Oak Ridge May NOI $339,150 actual and $353,200 budget, even though the
same rows interpreted as costs imply $46,750 actual and $88,600 budget.

The public sample actual and budget expense rows now use positive costs. Three
sanitized standardized portfolio fixture expense rows were corrected for the
same reason. A regression test pins the corrected revenue, cost, and NOI
bridge. Variance
analysis refuses to issue a report when the net actual or budget expense total
is negative and records an `expense_sign_anomaly` gap. This is a refusal, not
an automatic sign flip: individual expense credits and the sign conventions
of real Yardi/ResMan exports still require source review.

The existing `/home/ubuntu/projects/plat-operations/boxscore/data/boxscore_demo.db`
was not modified. It still contains the old negative-expense sample rows and
must be rebuilt or explicitly reconciled on a protected copy before its older
NOI reports are used. The September synthetic adapter already uses positive
expense costs and is unaffected by the public sample correction.
