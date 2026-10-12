# plat-operations (BOXSCORE)

**v0.1 candidate:** use the [exact-cent workflow](docs/EXACT_CENTS.md) for canonical operating imports, corrections, and reports. Legacy commands retain floating-point storage.

A deterministic, checked-cent calculation core for multifamily operating numbers
(net operating income (NOI) variance, occupancy, delinquency, trailing twelve
months, close readiness), served to Claude as an agent that puts a citation on
every number. The engine, not the model, does the arithmetic, and each answer
carries a trace id and hashes you can check later.

New here? Start with the [QUICKSTART](QUICKSTART.md), and see the
[glossary](docs/GLOSSARY.md) for the terms.

## Run it without a toolchain

The exact-cent producer (`boxscore-exact`, including its Model Context Protocol
(MCP) stdio and HTTP servers) is packaged as a container image: one binary on a
distroless base, non-root, no shell. Nothing listens unless you start `serve`,
and nothing else needs a network. Three commands load a synthetic sample
month for "Sample Gardens" (24 units, 2026-05):

```bash
IMAGE=ghcr.io/paintbrushv/plat-operations:0.1.1
SAFE="--network none --read-only --cap-drop ALL --security-opt no-new-privileges"
docker run --rm $SAFE "$IMAGE"                                    # 1. prints the calculation catalog
docker run --rm $SAFE -v boxscore-data:/data "$IMAGE" init --database /data/exact.sqlite   # 2. create the database
docker run --rm $SAFE -v boxscore-data:/data -v "$PWD/docs/quickstart:/in:ro" "$IMAGE" \
  import-csv --database /data/exact.sqlite --actuals /in/actuals.csv --budgets /in/budgets.csv \
  --snapshot /in/snapshot.json --property "Sample Gardens" --period 2026-05 --units 24   # 3. import it
```

> **No published image yet.** Until the first tagged release is published to
> GHCR (the publish job runs only on a `v*` tag), build the image locally from
> the repository root and use it (`IMAGE=boxscore-exact:local`) instead. The
> build command is the one in the Dockerfile header, and the image CI check
> (`scripts/check_image.py`) builds and checks the image the same way:
>
> ```bash
> docker build --build-arg VERSION=0.1.1 --build-arg REVISION="$(git rev-parse HEAD)" -t boxscore-exact:local .
> IMAGE=boxscore-exact:local
> ```

The [QUICKSTART](QUICKSTART.md) continues from here: review the period, connect
Claude Desktop, ask why NOI missed budget, and verify the citation. It also has
the no-Docker path (build the binary with Rust). [docs/INSTALL.md](docs/INSTALL.md)
has every container command, `mcp` and `serve` included;
[docs/DATA_FLOW.md](docs/DATA_FLOW.md) is the data-flow description for IT
review.

## Talk to it from Claude

`boxscore-exact mcp` is an MCP server over stdio, one tool per calculation.
Add it to Claude Desktop with the configuration in
[docs/connectors/claude-desktop.md](docs/connectors/claude-desktop.md) (native
binary) or [INSTALL.md](docs/INSTALL.md#mcp-over-stdio-claude-desktop)
(container). The server tells the model to take numbers only from the engine,
to say so when the engine refuses, and to cite the function, trace id and
output hash with every number.

`boxscore-exact serve` serves the same calculations over HTTP (MCP at `/mcp`,
`POST /v1/calc/{function}`, an OpenAPI contract); see
[docs/connectors/remote-mcp.md](docs/connectors/remote-mcp.md). Scripts and
Claude Code use API keys. The claude.ai and ChatGPT remote connectors sign in
with OAuth through your own identity provider (IdP), which the server checks
by token introspection; that works with IdPs that offer RFC 7662
introspection (Okta, Keycloak), not yet with Entra ID or Auth0, and the
connector walk-throughs have not been tried against a real IdP. That page
states exactly where it stands.
Before you connect any assistant to real data, read what the client sends to the
model vendor in [docs/DATA_FLOW.md](docs/DATA_FLOW.md#5-what-the-agent-client-sends-to-the-model-vendor).

## What it computes

Twelve named, versioned functions, all in checked integer USD cents (rates are
exact ratios, never floats). "Needs database" means the host must start the
server or command with `--database`; the input can never name a path.

| function | what it answers | needs database |
|---|---|---|
| `describe` | what can I ask? Lists every function with its schemas and the import adapters | no |
| `noi_variance` | how far is actual NOI from budget, by revenue and expense, for the rows I give you | no |
| `import_period` | store this property-period as a new immutable revision (a correction names the one it replaces and why) | yes |
| `review_period` | what is the NOI bridge for this stored revision, what was excluded, what changed from the revision it corrects, and does anything need review | yes |
| `issue_report` | record an immutable report for this revision (never approval) | yes |
| `list_revisions` | what has been imported, in what order, and which reports were issued | yes |
| `occupancy` | what share of units is occupied | only for a stored revision |
| `economic_occupancy` | what share of market rent is in place | only for a stored revision |
| `delinquency` | how much is delinquent or prepaid, and as a share of in-place rent | only for a stored revision |
| `close_readiness` | which properties are ready to close a period, and what blocks the rest | yes |
| `t12` | the trailing twelve-month statement, with missing months flagged, never zero | yes |
| `get_provenance` | which call produced this citation, from what input, and which revision and report it touched | yes |

The contract, envelope and versioning policy are in
[docs/EXACT_CENTS.md](docs/EXACT_CENTS.md#calculation-catalog-and-versioning-policy).

## Provenance

Every call into the catalog gets a trace id and a one-line citation naming the
function and version, the trace id and the first 12 characters of the input and
output hashes. With a database, each call is also written to an append-only
table, so a number in a memo can be looked up and re-run: the same input to the
same function version always gives the same output hash. A citation proves what
the engine computed from what it was given; it does not prove the export was
right, it is an integrity check rather than access control, and it is not
approval (every report says `human_approval: not_granted`). See
[docs/PROVENANCE.md](docs/PROVENANCE.md).

## Developers

The Rust crate lives in [`boxscore/`](boxscore/). The exact-cent core and the
agent surface are described in the "Exact-cent core and the agent surface"
section of [boxscore/docs/ARCHITECTURE.md](boxscore/docs/ARCHITECTURE.md#exact-cent-core-and-the-agent-surface),
including how to add a calculation and a source adapter. Contributing and
security reporting: [CONTRIBUTING.md](CONTRIBUTING.md), [SECURITY.md](SECURITY.md).

### Legacy `boxscore` harness (floating-point money)

This is the developer and legacy path, not the exact-cent workflow above. The
`boxscore` binary and its commands (`ingest`, `analyze`, `server`, the TUI,
`ask`) predate the checked-cent core and keep money as `f64`/`REAL`
internally; they are excluded from the exact-cent workflow and from the agent
connectors. They remain for development and demos on synthetic data.

**Local-first, AI-native multifamily asset-management intelligence harness** — the Rust
crate lives in [`boxscore/`](boxscore/). It ingests standardized multifamily operating
data (general ledger (GL), budgets, rent roll, receivables, leasing, monthly actuals) into SQLite,
computes NOI variance / trailing-twelve-month (T12) reversion / close-readiness / a scored call-ledger, and
outputs grounded intelligence as markdown reports, an HTTP/JSON API, a terminal UI,
and natural-language answers.

It behaves like a sharp, skeptical multifamily asset-management analyst — **not** a
chatbot, a dashboard toy, or a reporting generator.

### What's in this repository

| Path | Contents |
|---|---|
| [`boxscore/`](boxscore/) | The Rust crate: ingestion connectors, variance/T12/close-readiness engine, call ledger, TUI, HTTP API, `ask` agent |
| [`boxscore/docs/`](boxscore/docs/) | Architecture, data contracts, ontology, security & privacy docs |
| [`docs/`](docs/) | Exact-cent contract ([EXACT_CENTS.md](docs/EXACT_CENTS.md)), [PROVENANCE.md](docs/PROVENANCE.md), [GLOSSARY.md](docs/GLOSSARY.md), connector setup in [`docs/connectors/`](docs/connectors/) |
| [`docs/quickstart/`](docs/quickstart/) | **Synthetic** sample property ("Sample Gardens") used by the [QUICKSTART](QUICKSTART.md) and `scripts/quickstart_check.sh` |
| [`boxscore/data/sample/`](boxscore/data/sample/) | **Synthetic** sample portfolio used by the demo scripts |
| [`boxscore/tests/fixtures/`](boxscore/tests/fixtures/) | **Synthetic** standardized CSV/parquet fixtures used by the test suite |
| [`boxscore/demo/`](boxscore/demo/) | Synthetic demo-data generators and seed scripts |

All sample data and fixtures in this tree are **synthetic**. No real portfolio,
tenant, vendor, or investor data is included.

### Quick start (legacy harness)

Requires Rust (stable).

```bash
cd boxscore
cp .env.example .env
cargo run -- init
cargo run -- ingest property --file data/sample/properties.csv
cargo run -- ingest gl-actuals --file data/sample/gl_actuals.csv
cargo run -- ingest gl-budgets --file data/sample/gl_budgets.csv
cargo run -- ingest rent-roll --file data/sample/rent_roll_snapshots.csv
cargo run -- ingest delinquency --file data/sample/delinquency_snapshots.csv
cargo run -- ingest leasing --file data/sample/leasing_snapshots.csv
cargo run -- analyze variance --property "Oak Ridge" --period "2026-05"
```

Or run the demo: `./scripts/demo.sh` — then `./scripts/portfolio_demo.sh` for the
multi-property standardized-lane demo, and `cargo run -- close-readiness --period 2026-06`.

### Verification

From `boxscore/`:

```bash
cargo check
cargo clippy --all-targets -- -D warnings
cargo test
cargo fmt --check
```

## Honesty section (read before adopting)

- **Money in the legacy harness is currently `f64`/`REAL` internally** (the
  exact-cent workflow above uses checked integer cents). This is a known, documented
  violation of the project's own engineering rules; migration to fixed-point is a
  domain decision, not done. See `boxscore/ASSUMPTIONS.md`.
- **Validation is synthetic-only.** The bundled fixtures and demo data are
  fabricated; no real portfolio data has been used to validate this public tree.
- **No production connectors.** Not a real Yardi/Entrata/banking connector; the
  standardized-CSV contract is documented in `boxscore/docs/DATA_CONTRACTS.md`.
- **Grounded-or-it-doesn't-ship.** Every material claim in a generated report is
  meant to carry source, logic, assumptions, a confidence label, and a recommended
  next action; if the data doesn't support a conclusion, it becomes a question.
- **Human-in-the-loop.** Investor/lender reports, PM-performance criticism, and
  fraud/waste claims are never auto-sent.

## License

Apache-2.0 — see [LICENSE](LICENSE).