# Start here

For an asset-management analyst with no context on this repository. No
command line is needed unless you pick the third way in.

## What this is

You ask an AI assistant about property operating numbers in plain English,
and an engine called boxscore does the arithmetic in checked cents, never the
AI model. Every number comes back with a citation you can hand to someone
else so they can verify it.

## Three ways in

Least effort first. Only the first one needs IT.

1. **Your firm's claude.ai or ChatGPT connector. Needs IT.** IT runs the
   server, connects it to your firm's sign-in and loads the data. You add the
   connector in claude.ai or ChatGPT and sign in (once IT registers it, the
   server's browser page can use the same firm sign-in). IT's setup guide:
   [docs/connectors/remote-mcp.md](docs/connectors/remote-mcp.md). Its
   claude.ai and ChatGPT steps have not yet been tried with a real account,
   and that page says so.
2. **Claude Desktop on your own machine.** No IT, if you may install
   software. You need Docker or the `boxscore-exact` program, a database to
   point it at, and one edit to Claude Desktop's settings file:
   [docs/connectors/claude-desktop.md](docs/connectors/claude-desktop.md).
3. **The container quickstart.** No IT, if you have Docker. About ten
   minutes at the command line: it loads a synthetic sample property, "Sample
   Gardens" (24 units, period 2026-05), then connects Claude Desktop:
   [QUICKSTART.md](QUICKSTART.md).

Once connected, pick the `start_here` prompt where your client lists MCP
prompts, or type the first question below.

## The first three questions

Ask these, word for word. The `start_here` prompt offers the same three.

<!-- start-here-questions: these three lines must equal START_HERE_QUESTIONS in boxscore/src/exact/mcp.rs. A test (boxscore/tests/start_here_doc.rs) fails if they differ. -->
```text
What is loaded?
Why did NOI miss budget for Sample Gardens in 2026-05?
What does review_required mean here?
```

The answers below are from a real run of the engine on the synthetic sample.
The assistant's wording will differ from run to run. The figures must not.

**1. What is loaded?** The assistant calls `describe`, then `list_revisions`
with no filter. It lists each property, its periods, the latest revision
number, and whether a report has been issued. After the quickstart's first
import:

> Sample Gardens, 2026-05: revision 1, 24 units, no report issued yet.

**2. Why did NOI miss budget for Sample Gardens in 2026-05?** The assistant
finds the revision and calls `review_period`. Net operating income (NOI) was
13,700.00 against a budget of 18,800.00, a miss of 5,100.00
(`noi_variance` -5100.00). Revenue was 3,500.00 under budget (rent 2,400.00,
concessions 800.00, bad debt 400.00, other income 100.00 over). Expenses were
1,600.00 over (repairs and maintenance 1,000.00, payroll 500.00, utilities
200.00, management fee 100.00 under). It cites the call like this:

```
boxscore review_period@1.0.0 · trace 1616f452-846f-43c6-acd4-72f1ba5e0a46 · input sha256:23221cf93cc7… · output sha256:b21f9caf477c… · revision 1a303724-9e7a-42b8-8535-e22784865771
```

Yours differs: the trace id is new on every call, and the revision id and
both hashes are new on every database.

**3. What does review_required mean here?** The assistant explains the status
and the review reasons for what is loaded. For the sample May with its
occupancy snapshot, the review is `calculated` with no reasons, so nothing is
flagged. The quickstart's
[review_required section](QUICKSTART.md#what-review_required-means) imports
April without a snapshot. That review comes back `review_required` with `missing_occupancy_snapshot`: no occupancy
snapshot was imported, so import the month again with one, as a correction.

## How to read a citation

```
boxscore review_period@1.0.0 · trace <id> · input sha256:<12 hex>… · output sha256:<12 hex>… · revision <id>
```

| part | what it tells you |
|---|---|
| `review_period@1.0.0` | the calculation and its exact version |
| `trace <id>` | the id of this one call, used to look it up again |
| `input sha256:…` | the first 12 characters of a fingerprint of what the engine was given |
| `output sha256:…` | the first 12 characters of a fingerprint of what it returned |
| `revision <id>` | the stored version of the property and month it read (the revision number, 1, 2 and so on, is in `list_revisions`) |

To verify, someone with access to the database runs one command:

```bash
boxscore-exact trace --database ./exact.sqlite --id TRACE_ID
```

It shows the recorded calculation, version, fingerprints and revision, with
the property, period and source files. Check that they match the citation.
Running `review_period` again on the same revision gives the same output
fingerprint. In a shared deployment, ask IT, or ask the assistant to look up
the trace id (it calls `get_provenance`). A citation proves what the engine
computed from what it was given, not that the export was right. Details:
[docs/PROVENANCE.md](docs/PROVENANCE.md).

## What review_required means

Every answer has a status. `calculated`: done, nothing flagged.
`review_required`: done, the numbers are returned, and a person should look
before relying on them. It is not an error. `refused`: the engine did not
compute, and says why with an error code. The common reasons:

- `missing_actuals` / `missing_budgets`: one side of the month has no rows.
- `missing_occupancy_snapshot`: no occupancy snapshot was imported.
- `incomplete_account_coverage`: an account has an actual but no budget, or the reverse; it is left out of the NOI bridge and listed.
- `unmapped_accounts`: a line is marked `unmapped`, so it is not in NOI.
- `negative_net_expenses`: expenses add up below zero, usually a sign problem in the export.
- `missing_periods`: a twelve-month statement found a month with nothing loaded; it is shown as empty, never as zero.
- `down_units_present`: some units are offline. Information only.

Nothing is ever approved by the engine: every review and report says
`human_approval: not_granted`. Every reason is in
[docs/GLOSSARY.md](docs/GLOSSARY.md#review-reasons).

## Loading your own export

The profile for exports today is `canonical.csv/1`: a general ledger (GL)
actuals CSV and a GL budget CSV for one property and month, plus an optional
occupancy snapshot. (`canonical.json/1` exists for scripts.) No property
management system connector ships, so your export must be mapped to this
layout first:

```text
account_code,account_name,category,amount
4000,Gross Potential Rent,rental income,36000.00
4010,Concessions,concessions,-1200.00
```

- The header is exactly that line.
- Amounts are plain numbers with at most two decimals. No commas, currency
  signs or parentheses.
- Expenses are positive. Concessions and bad debt are negative.
- `rental income`, `concessions`, `bad debt` and `other income` are revenue.
  `unmapped` is kept out of NOI. Any other category is an expense.

In a shared deployment, loading data is IT's job. The full rules and what
refuses are in [QUICKSTART.md](QUICKSTART.md#load-your-own-export) and
[boxscore/docs/DATA_CONTRACTS.md](boxscore/docs/DATA_CONTRACTS.md#adapters-exact-cent-ingestion).

## What the assistant will not do

- Compute, round or restate a number the engine did not return. If the
  engine refuses, it says so and asks.
- Change data without a correction and a reason. Stored months are never
  edited. A fix is a new revision that names the one it replaces and says why.
- Send anything anywhere. The engine is local-first: no telemetry, no
  update check, and no outbound connection, except that a shared server checks
  sign-ins with your firm's identity provider. Your AI client does send the
  tool results, numbers included, to its model vendor; see
  [docs/DATA_FLOW.md](docs/DATA_FLOW.md#5-what-the-agent-client-sends-to-the-model-vendor).
- Approve anything. Sign-off is a person's job.

Terms: [docs/GLOSSARY.md](docs/GLOSSARY.md).
