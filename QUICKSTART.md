# Quickstart

This gets you from nothing to a cited answer in about ten minutes. You will
load one synthetic month for a made-up property, "Sample Gardens" (24 units,
period 2026-05), ask Claude why net operating income (NOI) missed budget, and
see how every number in the answer can be traced back to the engine that
computed it.

Analyst, not at a command line? Read [START_HERE.md](START_HERE.md) first.

What you need: either Docker (Path A) or a Rust toolchain (Path B), and Claude
Desktop if you want to ask questions in plain English. Everything here is
synthetic. The engine makes no network connection (what Claude Desktop itself
sends to Anthropic is covered in step 5). Commands are for a Unix-style shell (macOS, Linux, or WSL on
Windows) run from the root of this repository.

The files you will use are in [`docs/quickstart/`](docs/quickstart/):
`actuals.csv` and `budgets.csv` (ten general ledger (GL) accounts each, in the
canonical `account_code,account_name,category,amount` layout) and
`snapshot.json` (occupancy and rent roll totals at 2026-05-31).

## Path A: container (no toolchain)

Docker flags and every option below are explained in
[docs/INSTALL.md](docs/INSTALL.md); what the container reads, stores and
sends is in [docs/DATA_FLOW.md](docs/DATA_FLOW.md).

```bash
IMAGE=ghcr.io/paintbrushv/plat-operations:0.3.0
SAFE="--network none --read-only --cap-drop ALL --security-opt no-new-privileges"
docker pull "$IMAGE"
```

> **Published image.** Release 0.3.0 is pushed to GHCR as
> `ghcr.io/paintbrushv/plat-operations:0.3.0` (`linux/amd64`; the same digest is
> also tagged `sha-<first 7 hex of the commit>`, and there is no `latest` tag).
> The release workflow signs each image with cosign keyless signing and attaches
> its SBOM as a signed attestation. Verify both before first use with the
> commands in [Verify the image](docs/INSTALL.md#verify-the-image).
>
> To build the image yourself instead, skip `docker pull` and run this from the
> repository root (the command is the one in the Dockerfile header, and
> `scripts/check_image.py` builds and checks the image the same way):
>
> ```bash
> docker build --build-arg VERSION=0.3.0 --build-arg REVISION="$(git rev-parse HEAD)" -t boxscore-exact:local .
> IMAGE=boxscore-exact:local
> ```
>
> With the local image, put `boxscore-exact:local` in place of
> `ghcr.io/paintbrushv/plat-operations:0.3.0` in the Claude Desktop snippet in
> step 5.

**1. Describe the catalog.** This is the default command. It prints one JSON
envelope with `"status":"calculated"` listing the 13 calculations:

```bash
docker run --rm $SAFE "$IMAGE"
```

**2. Create the database once**, in a named volume:

```bash
docker run --rm $SAFE -v boxscore-data:/data "$IMAGE" init --database /data/exact.sqlite
```

**3. Import the sample month.** The folder with the CSV files is mounted
read-only at `/in`. On Linux the container user (uid 65532) must be able to read
it: `setfacl -R -m u:65532:rX docs/quickstart` (see INSTALL.md):

```bash
docker run --rm $SAFE -v boxscore-data:/data -v "$PWD/docs/quickstart:/in:ro" "$IMAGE" \
  import-csv --database /data/exact.sqlite \
  --actuals /in/actuals.csv --budgets /in/budgets.csv --snapshot /in/snapshot.json \
  --property "Sample Gardens" --period 2026-05 --units 24
# {"contract_version":"plat.ops/1","revision_id":"REVISION_ID","status":"imported"}
```

Copy the `revision_id` it prints; the next steps call it `REVISION_ID`.

**4. Review the period:**

```bash
docker run --rm $SAFE -v boxscore-data:/data:ro "$IMAGE" \
  review --database /data/exact.sqlite --revision REVISION_ID
```

You should see `"status":"calculated"` and, in `variance.noi_bridge`,
`"actual_noi":"13700.00"`, `"budget_noi":"18800.00"` and
`"noi_variance":"-5100.00"`.

**5. Connect Claude Desktop.** Add this to `claude_desktop_config.json` (macOS:
`~/Library/Application Support/Claude/claude_desktop_config.json`, Windows:
`%APPDATA%\Claude\claude_desktop_config.json`) and restart Claude Desktop:

```json
{
  "mcpServers": {
    "boxscore-exact": {
      "command": "docker",
      "args": [
        "run", "-i", "--rm",
        "--network", "none", "--read-only", "--cap-drop", "ALL",
        "--security-opt", "no-new-privileges",
        "-v", "boxscore-data:/data",
        "ghcr.io/paintbrushv/plat-operations:0.3.0",
        "mcp", "--database", "/data/exact.sqlite"
      ]
    }
  }
}
```

If Claude Desktop cannot find `docker`, put its absolute path in `command`.
This snippet is the one in
[INSTALL.md](docs/INSTALL.md#mcp-over-stdio-claude-desktop); the tools it
exposes are listed in [docs/connectors/claude-desktop.md](docs/connectors/claude-desktop.md).
Before you connect a client to real data, read
[what the client sends to the model vendor](docs/DATA_FLOW.md#5-what-the-agent-client-sends-to-the-model-vendor):
Claude Desktop sends the tool results it receives, numbers included, to
Anthropic.

**6. Ask:**

> Why did NOI miss budget in May for Sample Gardens?

Claude typically finds the revision (`list_revisions`) and reviews it
(`review_period`). Its wording will differ from run to run, but the figures must come from the
engine and match step 4: NOI of 13,700.00 against a budget of 18,800.00, a miss
of 5,100.00, made up of revenue 3,500.00 under budget (rent 2,400.00, concessions
800.00, bad debt 400.00, other income 100.00 over) and expenses 1,600.00 over
(repairs and maintenance 1,000.00, payroll 500.00, utilities 200.00, management
fee 100.00 under). The server tells Claude to cite the function, trace id and
output hash with every number and to say so if the engine refused rather than
compute anything itself.

**7. What a cited answer looks like.** Each tool result carries a one-line
`citation`. To see exactly what Claude saw, call the same function yourself. It
needs the volume writable, because the call is recorded in the database
(that is what makes it verifiable later):

```bash
echo '{"revision_id":"REVISION_ID"}' | docker run --rm -i $SAFE -v boxscore-data:/data "$IMAGE" \
  call --database /data/exact.sqlite --function review_period
```

The envelope's `citation` line looks like this (this one is from a real run
against the sample; your ids and hashes will differ):

```
boxscore review_period@1.0.0 · trace 702e281e-c352-41a5-8fad-3774c5b782fc · input sha256:622236c36b20… · output sha256:b82de9474840… · revision 6b373dcf-a9e8-432a-8dd1-7ef0511b9bb0
```

Read it as: the function and exact version that computed the number, the trace
id that finds the call again, the first 12 characters of the input and output
hashes, and the revision it read. [Verify a citation](#verify-a-citation) shows
how to check one.

> **Verification status.** The Docker commands on this page are not run by this
> repository's quickstart check, which has no Docker. The default `describe`, `init`,
> `import-csv` and `review` forms are copied from
> [INSTALL.md](docs/INSTALL.md), where the image CI check
> (`scripts/check_image.py`) runs them in the container with a different
> synthetic dataset and no `--snapshot`. Here the mount, arguments and
> `--snapshot` are changed to this sample's, and step 7 (`call` on a writable
> volume) and the Claude Desktop snippet (step 5, unchanged from INSTALL.md)
> are not run in a container by this quickstart's check. The same arguments are
> run against the native binary by `scripts/quickstart_check.sh`, so the numbers
> quoted above are checked, but the exact Docker command lines with this
> sample are not.

## Path B: native binary (no Docker)

Build once (needs Rust stable; first build takes a few minutes), then put the
binary on your `PATH`:

```bash
(cd boxscore && cargo build --locked --release --bin boxscore-exact)
export PATH="$PWD/boxscore/target/release:$PATH"

echo '{}' | boxscore-exact call --function describe       # "status":"calculated"
boxscore-exact init --database ./exact.sqlite
# {"contract_version":"plat.ops/1","schema_version":2,"status":"created"}

boxscore-exact import-csv --database ./exact.sqlite \
  --actuals docs/quickstart/actuals.csv --budgets docs/quickstart/budgets.csv \
  --snapshot docs/quickstart/snapshot.json \
  --property "Sample Gardens" --period 2026-05 --units 24
# {"contract_version":"plat.ops/1","revision_id":"6b373dcf-a9e8-432a-8dd1-7ef0511b9bb0","status":"imported"}
```

Review it, replacing `REVISION_ID` with the id you got:

```bash
boxscore-exact review --database ./exact.sqlite --revision REVISION_ID
```

The output is one line of JSON. The lines that matter, from a real run:

```
"status":"calculated"
"noi_bridge":{"actual_expenses":"21400.00","actual_noi":"13700.00","actual_revenue":"35100.00","budget_expenses":"19800.00","budget_noi":"18800.00","budget_revenue":"38600.00","expense_variance":"1600.00","noi_variance":"-5100.00","revenue_variance":"-3500.00", ...}
"review_reasons":[]
"human_approval":"not_granted"
```

Get the cited envelope, the same one Claude receives:

```bash
echo '{"revision_id":"REVISION_ID"}' | boxscore-exact call --database ./exact.sqlite --function review_period
```

**Connect Claude Desktop** with the native binary. Use absolute paths, since
Claude Desktop does not run in your shell:

```json
{
  "mcpServers": {
    "boxscore-exact": {
      "command": "/absolute/path/to/plat-operations/boxscore/target/release/boxscore-exact",
      "args": ["mcp", "--database", "/absolute/path/to/plat-operations/exact.sqlite"]
    }
  }
}
```

Restart Claude Desktop and ask the question from Path A step 6.

> **Verification status.** Every command in this section, and in the sections
> below that use the native binary, is run by
> [`scripts/quickstart_check.sh`](scripts/quickstart_check.sh), which asserts
> the status, the NOI figures quoted here and that a citation line is present.
> CI runs it on every change to this file or to `boxscore/`. It also drives
> `boxscore-exact mcp` over stdio with the same JSON-RPC calls Claude Desktop
> makes. It cannot run Claude Desktop itself, so the config file edit and the
> conversation are the parts nobody automated.

## Load your own export

The engine does not read vendor layouts. Map your general ledger export to the
canonical layout first, then import it through an **adapter profile**. List the
profiles and exactly what each accepts and refuses with:

```bash
boxscore-exact adapters
```

| profile | use it when | files | options |
|---|---|---|---|
| `canonical.csv/1` | you have a GL actuals CSV and a GL budget CSV (the usual case) | `actuals`, `budgets`, optional `snapshot` (JSON) | `property`, `period` (`YYYY-MM`), `units` |
| `canonical.json/1` | a script produces the whole period as one JSON document | `input` | none |

`import-csv` is shorthand for `import-with --profile canonical.csv/1`. The
long form is what you use for any adapter, including ones added later:

```bash
boxscore-exact import-with --database ./exact.sqlite --profile canonical.csv/1 \
  --file actuals=docs/quickstart/actuals.csv --file budgets=docs/quickstart/budgets.csv \
  --file snapshot=docs/quickstart/snapshot.json \
  --option property="Sample Gardens" --option period=2026-05 --option units=24
```

Running the same import again with the same files is harmless: it returns the
same `revision_id`.

What the CSV must look like:

- The header is exactly `account_code,account_name,category,amount`.
- `amount` is plain decimal text with at most two decimal places: `1200.00`,
  `-5.01`, `0.00`. No thousands separators, currency symbols or parentheses.
- Sign convention: expenses are **positive costs**; credits to an expense and
  contra-revenue (concessions, bad debt) are **negative**. The engine preserves
  signs and flags a negative net expense. It does not guess which way your
  export signs things.
- `category` decides the line's role. `rental income`, `concessions`, `bad debt`
  and `other income` are revenue; `unmapped` is reported separately and left
  out of NOI; any other category is an expense. Case and surrounding spaces in
  a category are ignored.
- An `account_code` must not have leading or trailing spaces, and one code must
  keep one category throughout the file.
- Each file is capped at 2 MiB and 100,000 rows combined.
- The snapshot is a JSON object (see `docs/quickstart/snapshot.json`) dated
  inside the period, whose occupied, vacant and down units add up to `units`.

What refuses (the import exits 2 and stores nothing), each one run against this
sample:

| you did | result |
|---|---|
| a CSV with other headers (`docs/quickstart/not_canonical.csv`) | `INVALID_INPUT` |
| `1,200.00`, `$1200.00`, `(1200.00)` or `1200.005` as an amount | `INVALID_INPUT` |
| a space before an account code | `INVALID_INPUT` |
| one account code under two categories | `ACCOUNT_MAPPING_CONFLICT` |
| a second, different import for a property and period that already has one, without `--supersedes` | `STALE_REVISION` |

Other refusals (`SNAPSHOT_MISMATCH`, `UNKNOWN_FILE`, `INPUT_LIMIT` and more) are
listed per profile by `boxscore-exact adapters`. One thing does **not** refuse:
a row repeated in a file is added twice (two identical 1,200.00 rent rows
became 2,400.00 in a test). Reconcile duplicates in the source before
importing.

An export from your property management system (PMS) needs a mapping step that
you do or review yourself: no vendor connector ships. Writing an adapter for a layout is described in
[boxscore/docs/DATA_CONTRACTS.md](boxscore/docs/DATA_CONTRACTS.md#adapters-exact-cent-ingestion).

## What `review_required` means

Every result has a status. `calculated` means the arithmetic finished and the
review found nothing to flag. `review_required` means the arithmetic finished
and **a person must look before relying on the number**. It is never an error
and never a refusal, and the numbers are still returned. `refused` means the
engine did not compute at all, with a typed `error.code`. Nothing is ever
approved by the engine: every report says `"human_approval":"not_granted"`.

The reasons appear in `review_reasons`. Here is April of the sample, imported
without a snapshot:

```bash
boxscore-exact import-csv --database ./exact.sqlite \
  --actuals docs/quickstart/actuals.csv --budgets docs/quickstart/budgets.csv \
  --property "Sample Gardens" --period 2026-04 --units 24
boxscore-exact review --database ./exact.sqlite --revision APRIL_REVISION_ID
# "status":"review_required" ... "review_reasons":["missing_occupancy_snapshot"]
```

| reason | raised when | what to do |
|---|---|---|
| `missing_actuals` / `missing_budgets` | one side has no rows | import the missing file |
| `incomplete_account_coverage` | an account has an actual but no budget, or the reverse; the NOI bridge covers only matched accounts and lists the rest under `excluded_accounts` | map the account on both sides, or accept the gap knowingly |
| `missing_occupancy_snapshot` | no snapshot was imported | import it with a snapshot, as a correction of the period |
| `unmapped_accounts` | a line has category `unmapped` | map it and correct the period |
| `negative_net_expenses` | an expense total is below zero | check the sign convention of the export |
| `missing_periods` | `t12` found a month with no revision | import the month; it is never filled with zero |
| `down_units_present` | `occupancy`: some units are offline (informational) | none required |
| `in_place_exceeds_market` | `economic_occupancy`: in-place rent is above market rent | check the rent roll |
| `missing_prepaid` / `missing_in_place_rent` | `delinquency`: no prepaid amount, or no usable in-place rent | supply it in the snapshot |
| `no_revisions_for_period` / `properties_not_ready` | `close_readiness`: nothing imported for the period, or a property is not ready | see that function's `properties` |

Definitions of each are in [docs/GLOSSARY.md](docs/GLOSSARY.md#review-reasons).

## Correct a period

Revisions are immutable. To fix a month, import the corrected file as a new
revision that names the one it replaces and says why. Both `--supersedes` (the
current revision id) and `--reason` are required, on any import command:

```bash
boxscore-exact import-csv --database ./exact.sqlite \
  --actuals docs/quickstart/actuals_corrected.csv --budgets docs/quickstart/budgets.csv \
  --snapshot docs/quickstart/snapshot.json \
  --property "Sample Gardens" --period 2026-05 --units 24 \
  --supersedes REVISION_ID --reason "Concessions credit of 400.00 was posted twice in the PMS export"
```

Reviewing the new revision reports what changed against the one it replaced:

```
"noi_variance":"-4700.00"          (in variance.noi_bridge)
"changes":{"actual_noi":"400.00", ... "noi_variance":"400.00", ...}
"correction_reason":"Concessions credit of 400.00 was posted twice in the PMS export"
"supersedes_revision":"<the original revision id>"
```

The original revision still reviews to `-5100.00`. If you give a stale revision
id (someone else already corrected the period) or no reason, the import refuses
with `STALE_REVISION`. To publish, `boxscore-exact issue --database ./exact.sqlite
--revision NEW_REVISION_ID` creates a new immutable report record with a new
report id each time; old report bodies never change. Issuing the original
revision now refuses with `SUPERSEDED`, naming the correction as the latest.

## Verify a citation

Anyone holding a citation (from Claude, a memo or an email) and read access to the
database can check it:

1. **Find the call.** The citation names a trace id:

   ```bash
   boxscore-exact trace --database ./exact.sqlite --id TRACE_ID
   ```

   The result (`get_provenance`) shows the recorded function, version, hashes,
   status, and the revision it read, with that revision's property, period,
   source file hashes, correction reason and parent. An unknown id refuses with
   `NOT_FOUND`. Check that the function, version and hash prefixes match the
   citation.
2. **Re-run the number.** Calling `review_period` again with the same
   `revision_id` returns the same `output_sha256`. The quickstart check asserts
   this. A review also re-checks the stored data against its input hash and
   refuses with `INTEGRITY_ERROR` if a stored row was altered.
3. **Read the revision.** `list_revisions` and `review_period` show every
   revision, its parent, its reason and its issued reports.

A citation proves what the engine computed from what it was given. It does not
prove the export was complete or correctly mapped, and it is an integrity check,
not access control. The limits, and the `BOXSCORE_TRACE_INPUTS` and
`BOXSCORE_TRACE_DIR` settings, are in [docs/PROVENANCE.md](docs/PROVENANCE.md).

## Where next

- [docs/INSTALL.md](docs/INSTALL.md): the container, volumes, backup, upgrade and
  the `serve` HTTP server.
- [docs/DATA_FLOW.md](docs/DATA_FLOW.md): what goes in, what is stored, what
  leaves, for an IT review.
- [docs/connectors/remote-mcp.md](docs/connectors/remote-mcp.md): serve the
  catalog over HTTP with API keys (scripts and Claude Code) or OAuth (the
  standard web sign-in protocol, for remote claude.ai and ChatGPT
  connectors, through your own identity provider; not yet tried against a
  real one); that page says exactly where it stands.
- [docs/EXACT_CENTS.md](docs/EXACT_CENTS.md): the full contract: the 12
  functions, the envelope, versioning.
- [docs/GLOSSARY.md](docs/GLOSSARY.md): every term used here.
