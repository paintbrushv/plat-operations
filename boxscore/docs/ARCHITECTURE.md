# Architecture

Boxscore separates the replaceable model from the durable product harness.

This document has two halves. The sections from "Layers" to "Safety Boundary"
describe the legacy harness (floating-point money, `boxscore` binary). The
section [Exact-cent core and the agent surface](#exact-cent-core-and-the-agent-surface)
at the end describes the checked-cent core (`boxscore-exact`) that the agent
connectors serve; start there for the Claude and ChatGPT path.

## Layers

- CLI and API: Clap commands and Axum JSON endpoints.
- Persistence: SQLx with SQLite by default.
- Data registry: normalized tables for properties, periods, GL actuals/budgets, rent roll, delinquency, and leasing snapshots.
- Tool registry: typed metadata and logged tool runs for every analysis stage.
- Variance engine: deterministic revenue, expense, and NOI variance calculations.
- Gap engine: identifies missing data and low-confidence analysis areas.
- Question engine: turns gaps into prioritized operator questions.
- Memory layer: records specific reusable facts with confidence scores.
- Evolution engine: turns repeated gaps and missing data into capability proposals.
- Report writer: creates evidence-aware markdown reports under `reports/generated/`.

## Model Boundary

`ModelProvider` is a trait. v0.1 ships with `StubModelProvider`, which is deterministic and local. Future providers can implement OpenAI-compatible, Anthropic, or local-model adapters without changing the harness.

## Safety Boundary

Boxscore may propose capabilities, migrations, tools, and questions. It must not silently self-modify production code, delete data, transmit private data externally, or deploy.

## Exact-cent core and the agent surface

The exact-cent core is the `boxscore::exact` module tree (`boxscore/src/exact/`)
and the `boxscore-exact` binary (`boxscore/src/bin/boxscore-exact.rs`). It uses
checked signed integer USD cents, never floating point; the legacy harness
keeps its floating-point money. Its contract is [EXACT_CENTS.md](../../docs/EXACT_CENTS.md);
how a number is traced is [PROVENANCE.md](../../docs/PROVENANCE.md); the terms
are in [GLOSSARY.md](../../docs/GLOSSARY.md).

### Modules

| module | role |
|---|---|
| `money` | `Money`, signed cents in an `i64`; decimal-string parsing (at most two places) and checked add and subtract. Overflow refuses, never wraps |
| `ratio` | `Ratio`: integer numerator and denominator plus a decimal rounded half away from zero at scale 4. No floats |
| `variance` | the NOI (net operating income) bridge over actual and budget rows, and its `negative_net_expenses` review reason |
| `store` | the SQLite store: schema, immutable revisions, snapshots, issued reports, corrections, `review`, `issue`, hash verification on load, UPDATE and DELETE refusal triggers |
| `ingest` | source adapters: the `SourceAdapter` trait, profile ids, specs, the registry and `read_with`; ships `canonical.csv/1` and `canonical.json/1` |
| `catalog` | the named, versioned calculations, their JSON Schemas, the envelope and `invoke`, the single entry point (below) |
| `snapshot_metrics` | `occupancy`, `economic_occupancy` and `delinquency` over one snapshot |
| `readiness` | `close_readiness`: per-property period readiness from the store |
| `t12` | the trailing twelve-period statement; a missing month is null, never zero |
| `trace` | the append-only `exact_invocations` table or JSONL trace, the citation line, and `get_provenance` |
| `mcp` | MCP (Model Context Protocol) server over stdio, hand-rolled on `serde_json` |
| `mcp_http` | MCP Streamable HTTP, `POST /v1/calc/{function}`, `/openapi.json`, API keys and rate limiting, on axum |
| `protocol` | the bounded stdin/stdout JSON request used by Python hosts (`boxscore-exact protocol`) |
| `migration` | the reviewed, source-preserving copy migration from a legacy database |

### The catalog is the boundary

`catalog::CATALOG` is the one list of what the engine can be asked. Every
transport reaches the engine only through `catalog::invoke` (or
`invoke_hashed`, when the transport hashed the raw bytes), which:

1. resolves the function and, if pinned, its exact version (`UNKNOWN_FUNCTION`,
   `VERSION_MISMATCH`);
2. parses the input against the function's typed input, rejecting unknown
   fields; the input has no path, SQL or network field, and the database path
   and `caller` are set by the host process, never parsed from the input;
3. runs the calculation, then writes the trace record after the calculation's
   own writes have committed;
4. returns the envelope: contract version, function, version, producer, input
   and output hashes, trace id, citation and either a `result` or a typed
   `error`.

The legacy floating-point calculations are not in the catalog, so no agent can
reach them. The summary and schemas of each entry feed the MCP tool list and
the OpenAPI document automatically; nothing is hand-copied into a transport.

### Two transports, one dispatcher

- **stdio** (`boxscore-exact mcp [--database PATH]`): JSON-RPC 2.0, one message
  per line, for Claude Desktop and other local MCP clients. No network.
- **HTTP** (`boxscore-exact serve`): MCP Streamable HTTP at `/mcp`,
  `POST /v1/calc/{function}`, `/openapi.json`, `/health`, and the OAuth
  protected-resource metadata. Loopback only unless an API key file or OAuth
  (RFC 7662 introspection, `exact/oauth.rs`) is configured; both sit behind
  the async `TokenValidator` trait. It reuses the stdio dispatcher
  (`mcp::Server::handle`) unchanged.

Both call `catalog::invoke`, so the same call returns the same `result` and
`output_sha256` on either. The conformance suite
(`boxscore/tests/exact_conformance.rs` with the corpus in
`boxscore/tests/fixtures/catalog/`) enforces it: every golden case runs through
`catalog::invoke`, the stdio server and both HTTP routes, and the four must
agree and match the pinned `expected.json` and the function's output schema.
Setup details: [Claude Desktop](../../docs/connectors/claude-desktop.md),
[remote MCP](../../docs/connectors/remote-mcp.md),
[OpenAPI](../../docs/connectors/openapi.md).

### Versioning policy

Versions are per function. A patch bump changes nothing observable in `result`,
a minor bump only adds fields, and a major bump may change a number for the
same input. Each golden pins the version it was generated at, so a changed
number under an unchanged version fails the conformance suite. The full policy
is in [EXACT_CENTS.md](../../docs/EXACT_CENTS.md#calculation-catalog-and-versioning-policy).

### How to add a calculation

1. **Write the calculation** as a pure function in a new or existing `exact::`
   module. Use `Money::checked_add` and `checked_sub` and `Ratio` only; return a
   typed `ExactError` code for refusals. Raise review reasons as strings the
   function returns; the catalog turns any reason into `review_required`.
2. **Add the entry** to `CATALOG` in `catalog.rs`: `name`, `version` (start at
   `1.0.0`), `summary`, `stateful` (true only if it reads or writes the store),
   and the `input_schema` and `output_schema` functions (JSON Schema 2020-12;
   money is a decimal string, never a number).
3. **Add the dispatch arm** in `catalog::execute`: parse the typed input with
   `parse`, take the database through `database(calc, call)` if stateful, call
   your function and return `(status, result)`. An entry with no arm returns
   `INTERNAL_ERROR`.
4. **Add a golden case**: `tests/fixtures/catalog/<function>/<case>.input.json`
   and `<case>.expected.json` (or a chain for anything that needs prior state),
   then `BOXSCORE_UPDATE_GOLDENS=1 cargo test --test exact_conformance` and
   review the generated file by hand. The conformance suite's coverage guard
   (`every_catalog_function_has_a_case`) fails, naming the function, until a
   case exists.
5. **Bump `describe`**: it lists the whole catalog, so adding a function changes
   its result. Make a minor bump (`1.1.0` to `1.2.0`) and regenerate its golden.
6. **Document it**: the table in `docs/EXACT_CENTS.md`, the tool table in
   `docs/connectors/claude-desktop.md`, and the glossary if it adds a review
   reason. The MCP tool, HTTP route and OpenAPI operation need no code.

### How to add a source adapter

An adapter maps a vendor file layout into the canonical dataset; it does not
guess signs, currency or account approval, and it must refuse rather than
coerce. The rules are in
[DATA_CONTRACTS.md](DATA_CONTRACTS.md#adapters-exact-cent-ingestion).

1. **Implement the trait** `ingest::SourceAdapter` in `boxscore/src/exact/` (or
   a submodule): `profile()` returns a versioned id of the form `name/N`;
   `spec()` returns an `AdapterSpec` naming every file with its expected header
   or shape, every option, and what makes it refuse; `read()` returns the
   `Dataset` and a `sources` object holding the SHA-256 of every file read.
2. **Pick the profile id carefully.** An id is exact, with no prefix match,
   default or "latest". Changing what an adapter accepts means a new id
   (`name/2`) registered next to the old one.
3. **Read files only through** `AdapterInputs::require_file` / `read_file`,
   which apply the 2 MiB cap. `ingest::read_with` checks inputs against the spec
   before reading (`UNKNOWN_FILE`, `MISSING_OPTION`, and so on) and stamps
   `sources.profile`.
4. **Register it** in the static `REGISTRY` in `ingest.rs`. After that
   `boxscore-exact adapters`, `import-with --profile` and the catalog's
   `describe` list it.
5. **Test it** in `tests/exact_ingest.rs`: import synthetic files through the
   profile, check `sources.profile`, review the result, and cover each refusal
   in the spec. Adding an adapter does not change the catalog, so no golden
   changes.
