# Data flow: the boxscore-exact container image

This page is for an IT or security reviewer who has to decide whether the
`boxscore-exact` container image (`ghcr.io/paintbrushv/plat-operations`) can
run on a company machine. It covers what enters the container, where it is
stored, what is computed, what leaves, how to back it up, delete it and
verify the image. How to run it is in [INSTALL.md](INSTALL.md).

The image contains one program, `boxscore-exact`, the exact-cent calculation
catalog described in [EXACT_CENTS.md](EXACT_CENTS.md). The repository's older
`boxscore` program (HTTP API, terminal UI, `ask` agent, legacy importers) is
not in the image.

Each claim is marked with how it is backed:

- **[CI]**: checked on every image build by the `Container image` workflow
  (`.github/workflows/image.yml`, which runs `scripts/check_image.py`). A
  release image is not pushed unless these checks pass.
- **[tests]**: checked by the Rust test suite (`cargo test`) on the binary,
  not on the image specifically.
- **[stated]**: true by reading the source code at the cited place, but no
  automated check proves it.

## Summary

| question | answer | backed by |
|---|---|---|
| Does it listen on a port? | Not by default. No `EXPOSE`; the default command and every subcommand except `serve` open no socket. `serve` listens only when you start it, refuses a non-loopback address without an API key file or OAuth configuration, and the documented setup publishes it to host loopback only. See section 4. | [CI] every other check runs with `--network none`; the `serve` check publishes to `127.0.0.1` only; [tests]; [stated] |
| Does it connect out? | No telemetry, no update check, no crash reporting. One exception, only when you configure OAuth on `serve`: it calls your identity provider's token introspection endpoint to check bearer tokens that are not API keys (and, without `--introspection-endpoint`, reads the provider's metadata once at start-up). Nothing else. | [CI] every check except `serve` runs with `--network none`; [tests]; [stated] |
| Where is data stored? | One SQLite file you name, on the `/data` volume, mode 0600, which also holds the invocation trace. Without a database, a JSONL trace file (mode 0600) only if you set `BOXSCORE_TRACE_DIR`. | [CI] for the database; [tests] and [stated] for the trace file |
| What else does it write? | Nothing. The root filesystem can be read-only. | [CI] runs with `--read-only`, `serve` included |
| Does it run as root? | No: uid/gid 65532. No shell in the image. | [CI] |
| Is data baked into the image? | No. `/data` ships empty; no database or data file is in the image. | [CI] |
| Does anything reach an AI vendor? | Not from the container. If you connect an agent client (Claude Desktop over stdio, or a remote client through `serve`), **the client sends tool arguments and results, including your numbers, to the model vendor.** See section 5. | [stated] |

## 1. What enters

| input | how | limit |
|---|---|---|
| Canonical GL exports: CSV with `account_code,account_name,category,amount` | a file path on a mount you choose (`import-csv`) | 2 MiB per file |
| A canonical period as JSON, an optional occupancy snapshot JSON | a file path (`import --input`, `import-csv --snapshot`) | 2 MiB per file |
| A legacy database and a signed-off review file for reviewed migration | file paths (`plan-migration`, `migrate`) | review file 2 MiB |
| A calculation request (`call`, `protocol`) | JSON on stdin | 2 MiB |
| MCP tool calls from an agent client (`mcp`) | JSON-RPC on stdin, one message per line | 2 MiB per line |
| HTTP requests to `serve`, only if you start it: `POST /mcp` (one JSON-RPC message), `POST /v1/calc/{function}` (that function's input) | over the network, on the address you bind and publish; a bearer token when a key file is given | 2 MiB per request body |
| API keys for `serve` (`key_id:secret` lines) | a file path (`serve --api-key-file`), mounted read-only | 1 MiB |
| Database path, property label, period, unit count | command-line arguments | |

The data itself is general-ledger account codes, account names, categories
and amounts, a property label, a period, a unit count and, optionally,
rent-roll totals (occupied/vacant/down units, market and in-place rent,
delinquent, prepaid and concession amounts). The canonical schema has no
tenant, resident, vendor or person field, and unknown fields are refused. Free
text fields (`account_name`, the property label) are stored as supplied, so
keep personal data out of them. The program does not read vendor systems,
URLs or databases other than the one you name. [stated:
`boxscore/src/bin/boxscore-exact.rs`, `boxscore/src/exact/store.rs`]

The program reads three environment variables: `BOXSCORE_MCP_DEBUG`
(diagnostics, section 2), `BOXSCORE_TRACE_DIR` and `BOXSCORE_TRACE_INPUTS`
(the invocation trace, section 2). It reads them from the host environment
only, never from a request, and it does not load `.env` files. [stated:
`boxscore/src/bin/boxscore-exact.rs` `main`, `boxscore/src/exact/mcp.rs`,
`boxscore/src/exact/trace.rs`]

## 2. Where it is stored

**One SQLite file**, at the path given by `--database`. The documented setup
puts it at `/data/exact.sqlite` on a Docker named volume.

- `init` creates it with exclusive create (an existing path is refused) and
  file mode **0600**, owned by the container user (uid 65532). [CI checks
  mode and owner of the file created in the volume]
- The `/data` directory in the image is owned by uid 65532 with mode 0700 and
  is empty. [CI]
- While a write is in progress SQLite keeps a rollback journal,
  `exact.sqlite-journal`, next to the database; it is removed when the write
  commits. The database does not use WAL mode. [stated: the connection sets
  no journal mode, so SQLite's default `DELETE` journal applies; CI checks
  that after a full init/import/review/MCP run the volume holds only
  `exact.sqlite`]
- Tables: `exact_revisions` (one row per imported period revision, with the
  SHA-256 of the canonical input and of the source files), `exact_gl` (the GL
  rows, amounts as integer cents), `exact_snapshots` and
  `exact_snapshot_money` (the optional occupancy snapshot), `exact_reports`
  (issued report bodies with their SHA-256), `exact_migrations` and
  `archived_reports` (reviewed migration records), and `exact_invocations`
  (one record per catalog call; see the invocation trace below). [stated:
  `boxscore/src/exact/store.rs`]
- **Append-only.** Every one of those tables has `BEFORE UPDATE` and
  `BEFORE DELETE` triggers that abort with `immutable exact record`.
  Corrections add a new revision; they never change an old one. [tests:
  `boxscore/tests/exact_store.rs` and, for `exact_invocations`,
  `boxscore/tests/exact_trace.rs` attempt an UPDATE and a DELETE and expect
  refusal] These triggers and the content hashes are integrity checks, not
  access control: anyone who can write the file can drop a trigger. Protect
  the volume with host permissions, disk encryption and backups.

**Migration output.** `migrate --destination DIR` writes a new directory
holding a byte-identical read-only copy of the legacy database, a new
`exact.sqlite` and `migration-report.json`. It writes only where you point it.
[stated: `boxscore/src/exact/migration.rs`]

**Invocation trace.** Every call to a catalog function (`call`, `trace`,
`protocol`, MCP tools over stdio or HTTP, and `/v1/calc`) gets a trace id
and one trace record, for successes and refusals alike. The record goes to
one place at most ([PROVENANCE.md](PROVENANCE.md) has the details):

- **With a database** (`--database`): a row in the `exact_invocations` table
  of that same SQLite file, append-only with the same UPDATE/DELETE refusal
  triggers as the other tables. [tests: `boxscore/tests/exact_trace.rs`]
- **Without a database, when `BOXSCORE_TRACE_DIR` names an existing
  directory**: one JSON line appended to `$BOXSCORE_TRACE_DIR/invocations.jsonl`,
  a file created with mode 0600. Nothing is written there when a database is
  in use or when the variable is unset. [tests: `boxscore/tests/exact_trace.rs`
  checks the line and that a missing directory is reported, not fatal;
  stated: the 0600 mode, `record_jsonl` in `boxscore/src/exact/trace.rs`]
- **Otherwise** nothing is written and the envelope says
  `trace_store: "none"`. [CI: the `serve` check makes a tool call with
  neither, sees `trace_store: "none"` and finds `/data` unchanged]

A record holds the trace id, function and version, producer version,
caller, the SHA-256 of the input and of the output, status, error code, the
revision and report ids it names, and start and finish times. The caller is
`cli`, `protocol` or `mcp` for local use, and `http:<key_id>` for an HTTP
caller: the id of the API key that authenticated the request, never the
secret (`http:local` for a loopback server without a key file). [stated:
`boxscore/src/exact/trace.rs`, `boxscore/src/exact/mcp_http.rs`]

**Raw inputs are not kept by default.** The record's `input_json` is empty
unless the operator sets `BOXSCORE_TRACE_INPUTS=1`, so by default a trace
holds hashes, ids and times but no operating numbers. With the variable set,
it also keeps the canonical JSON of each input, which includes your numbers,
in the database or the JSONL file. [tests: `boxscore/tests/exact_trace.rs`]

**Output streams.** Results go to stdout as JSON, or, for `serve`, in the
HTTP response. Diagnostics go to stderr only when `BOXSCORE_MCP_DEBUG` is set,
and they name MCP methods, never arguments or results. `serve` also prints
one line to stderr when it starts (the bound address and the auth mode), one
when it stops, a warning for a group-readable key file, and start-up
refusals; it logs no request content, token or result. [stated:
`boxscore/src/exact/mcp.rs`, `boxscore/src/exact/mcp_http.rs`; CI: the
`serve` container's logs do not contain the API key secret] Docker itself
keeps the stdout and stderr of a **detached** container (`docker run -d`) in
its log driver. For one-shot commands that would include results, so the
documented ones run attached with `--rm` and output goes only to your
terminal or to the MCP client. A detached `serve` container's log holds only
the stderr lines above.

## 3. What is computed

Deterministic arithmetic in checked signed 64-bit integer cents: account-level
actual versus budget variance, the NOI bridge (revenue, expenses, NOI and
their variances), review reasons (for example a missing occupancy snapshot,
accounts with only an actual or only a budget, unmapped accounts or negative
net expenses), correction bridges between revisions, occupancy, economic
occupancy and delinquency as exact four-place ratios, per-property close
readiness, trailing-twelve-period statements, and immutable report records;
`get_provenance` reads a trace record back. Every result carries `function@version`,
`input_sha256` and `output_sha256`, so a number can be reproduced from its
citation. Every review and report says `human_approval: not_granted`.

The container runs no model, no forecasting and no inference. It does not
contact an AI service. [stated: `boxscore/src/exact/`; [tests] cover the
arithmetic, overflow refusal and immutability]

## 4. What leaves the host

The container opens no connection of its own, with one exception that you
switch on: `serve` with OAuth configured calls your identity provider (see
**Outbound connections** below). With `serve` running, results go back only
as responses to the requests it receives.

**Listeners: none by default.** The image declares no `EXPOSE`, and the
default command and every subcommand except `serve` open no socket. `mcp`
speaks only over stdin and stdout of the process that started it. [CI: every
check except the `serve` check, including the stdio MCP session, runs with
`--network none`, so none of them needs a network; stated: the only socket
code reachable from `boxscore/src/bin/boxscore-exact.rs` and
`boxscore/src/exact/` is the HTTP listener in
`boxscore/src/exact/mcp_http.rs`, which `main` starts only for `serve`, and
the OAuth introspection client in `boxscore/src/exact/oauth.rs`, which
`serve` builds only when OAuth is configured]

**`serve`, only when you start it.** `boxscore-exact serve` is the HTTP
server for the catalog (MCP Streamable HTTP and a JSON API; see
[connectors/remote-mcp.md](connectors/remote-mcp.md)).

- It listens only when started explicitly with `serve`. Its default address
  is `127.0.0.1:3819`. [stated: `boxscore/src/bin/boxscore-exact.rs`]
- It refuses to bind a non-loopback address, or to advertise a non-loopback
  `--public-url`, unless at least one validator is configured: an API key
  file (`--api-key-file`), OAuth (`--authorization-server` with
  `--oauth-audience`, `--oauth-client-id` and `--oauth-client-secret-file`),
  or both. It refuses a key file or OAuth client secret file that other
  users can read or write, and any inconsistent combination of OAuth flags.
  [tests: `boxscore/tests/exact_mcp_http.rs`,
  `boxscore/tests/exact_mcp_http_oauth.rs`] Inside a container it must bind
  `0.0.0.0`, so a validator is required there; publish the port to the host
  loopback only (`-p 127.0.0.1:3819:3819`). [CI: the `serve` check runs it
  this way, with a 0600 key file mounted read-only; it runs the container as
  the CI user that owns that file, because handing the file to uid 65532
  needs root on the host (INSTALL.md does that with `chown`)]
- `/health`, `/openapi.json`, the OAuth protected-resource metadata
  (`/.well-known/oauth-protected-resource` and its `/mcp` variant) and the
  workbench page (`/`, `/app.css`, `/app.js`) answer without
  authentication. With a validator, everything else (`/mcp`,
  `/v1/calc/{function}`) needs `Authorization: Bearer <token>`: an API key or
  an OAuth access token. A missing or refused token gets `401` with a
  `WWW-Authenticate` challenge; an OAuth token without the required scope
  gets `403`. A loopback server started without a validator asks for no
  token. [CI: `/health` and the metadata answer `200` without a token,
  `POST /mcp` without one gets the `401` challenge and with an API key
  completes `initialize`; tests: `boxscore/tests/exact_mcp_http.rs`,
  `boxscore/tests/exact_mcp_http_oauth.rs`]
- A request whose `Origin` header is neither the server's own origin (next
  item) nor on the `--allow-origin` allowlist (empty by default) gets `403`,
  on every route. Each caller (API key or
  OAuth subject) is rate-limited (`--rate-limit`, default 60 requests a
  minute; `429` with `Retry-After`). Request bodies over 2 MiB get `413`.
  [tests: `boxscore/tests/exact_mcp_http.rs`]
- The server's own origin needs no `--allow-origin`: the origin of
  `--public-url` (default `http://127.0.0.1:<port>`); for a loopback public
  URL, its `127.0.0.1`, `localhost` and `[::1]` spellings with the same
  scheme and port; and for a loopback bind, `http://127.0.0.1:<port>`,
  `http://localhost:<port>` and `http://[::1]:<port>` on the bound port. A
  default port is matched as the browser sends it, without `:80` or `:443`.
  That is what lets the browser workbench below call the API it was served
  with. A port remapped by Docker or a proxy is not the server's own origin
  unless `--public-url` names it (INSTALL.md, Browser workbench). [tests:
  `boxscore/tests/exact_workbench.rs`; CI: the `serve` check calls
  `/v1/calc/describe` with its own origin and with another]
- **Browser workbench.** `GET /` serves a page (with `/app.css`
  and `/app.js`) on the same port and origin as the API. The three files are
  compiled into the binary; the page loads nothing from anywhere else (its
  Content-Security-Policy allows this origin only) and needs no new
  outbound connection. Loading the page needs no token; its data calls go to
  the authenticated `/v1/calc` routes like any other client's. An API key
  pasted into it is held in the browser's `sessionStorage` for the life of
  the tab, sent only as `Authorization: Bearer` to this server, and never
  put in a cookie or a URL. `sessionStorage` is copied into a duplicated
  tab, and any page served from the same origin can read it, so give the
  server an origin of its own (no path-prefix sharing with other apps). The
  `/v1/calc` and `/mcp` responses carry `Cache-Control: no-store` and
  `X-Content-Type-Options: nosniff`. The page lists the stored
  revisions, reviews one and looks up a trace; it can also import a period
  or a correction, sending the text of the files the user chooses to the
  server over that same connection (in an `import_period` request body, 2 MiB
  at most, never stored by the page); it cannot issue reports, and it does not
  support OAuth sign-in (use an API key, or the agent connectors). [tests:
  `boxscore/tests/exact_workbench.rs`; CI: the `serve` check fetches `/`
  and its Content-Security-Policy]
- It speaks plain HTTP. Terminate TLS at a reverse proxy in front of it.
  [stated]
- The database path comes from its command line, never from a request.
  [tests: `boxscore/tests/exact_mcp_http.rs`]

**Outbound connections: none, except OAuth token introspection when you
configure it.** No telemetry, no analytics, no update check, no crash
reporting, no licence server. [CI: every check except `serve` passes with
`--network none`; stated: as above]

`serve` with OAuth configured makes exactly one kind of outbound connection:
an HTTPS `POST` to the token introspection endpoint of the identity provider
you named (`--introspection-endpoint`, or the `introspection_endpoint` in
that provider's metadata). [tests: `boxscore/tests/exact_mcp_http_oauth.rs`;
stated: `boxscore/src/exact/oauth.rs`]

- **When:** only when a request carries a bearer token that is not one of the
  static API keys, and no cached verdict for that token is still valid
  (accepted tokens are cached until their expiry, at most 5 minutes;
  refused ones for 30 seconds). API key requests, requests without a token
  and public routes never cause one. Without `--introspection-endpoint`, the
  server also reads the issuer's RFC 8414 metadata at start-up to find the
  endpoint (one `GET`, or two when an issuer with a path serves it only at
  `<issuer>/.well-known/oauth-authorization-server`), and none afterwards.
- **What is sent:** the bearer token, `token_type_hint=access_token`, and the
  server's own client id and secret (HTTP Basic). No request body, tool
  argument, result, database content or caller address is sent.
- **What comes back:** the provider's verdict on the token (`active`, `exp`,
  `aud`, `scope`, `sub`, ...). The subject (`sub`, else `client_id`) is
  recorded as the trace caller `http:oauth:<subject>` in the database.
- **Limits:** 5-second timeout, no redirects followed, `https` required
  except to a loopback host, at most 64 KiB read back. The server makes at
  most 16 concurrent and 120 per minute introspection calls; anything beyond
  answers `503` without a call. The environment's
  `HTTPS_PROXY`/`HTTP_PROXY` apply. Neither the token nor the secret is
  logged; a failed call logs one line naming the endpoint and the kind of
  failure, at most one every 10 seconds.

Run with `--network none` (as the [INSTALL.md](INSTALL.md) examples do for
every command except `serve`) and the kernel enforces "no connections". A
`serve` container needs a network interface to accept connections, so for it
this is [stated], not enforced by `--network none`; use a host firewall rule
that allows only the introspection endpoint (OAuth) or nothing (API keys
only) if you need it enforced. `--network none` is incompatible with OAuth in
any case: the server could not reach the introspection endpoint, and every
OAuth request would get `503`.

**Files written: only the database** (and its transient journal), the JSONL
trace file `invocations.jsonl` when `BOXSCORE_TRACE_DIR` is set and no
database is in use, plus the migration directory if you run `migrate`. [CI:
the checks run with `--read-only`, so any other write would fail; the volume
holds only `exact.sqlite` afterwards; and a `serve` session leaves `/data` as
the default command left it]

The Docker daemon contacts `ghcr.io` when you pull the image. That is
Docker's traffic, not the container's.

## 5. What the agent client sends to the model vendor

**Read this before connecting the MCP server to an AI assistant.**

When the catalog is connected to an assistant (`boxscore-exact mcp` in
Claude Desktop, or `boxscore-exact serve` for a client that connects over
HTTP), the assistant decides which tool to call and with what arguments. The client application sends **the tool
call arguments and the full tool result** to the model vendor (Anthropic for
Claude, OpenAI for ChatGPT) as part of the conversation. That includes your
numbers: GL rows passed to `noi_variance` or `import_period`, review results
with account names, amounts and variances, and revision lists. It also
includes anything you type or paste into the chat. The container does not
send this; the client does, under your organisation's agreement with that
vendor (data retention, training use, region). The container cannot see or
limit it. [stated]

With a remote connector (claude.ai or ChatGPT through `serve`), the vendor's
servers call your endpoint, so the endpoint must be reachable from them; see
[connectors/remote-mcp.md](connectors/remote-mcp.md), which also explains
that those connectors sign users in with OAuth through your identity
provider, and that the claude.ai and ChatGPT walk-throughs there have not
been tried against a real provider. [stated]

How to limit what reaches the vendor:

- **Import outside the chat.** Run `import-csv` from the command line
  (see the one-liners in [INSTALL.md](INSTALL.md)), so raw exports never pass
  through the assistant. Results the assistant asks for, such as a review, still go
  to the vendor.
- **Run stateless when you can.** Start `mcp` without `--database`: only the
  stateless functions work (`describe`, `noi_variance`, and `occupancy`,
  `economic_occupancy` and `delinquency` on an inline snapshot), nothing is
  stored apart from a JSONL trace if you set `BOXSCORE_TRACE_DIR`, and the
  assistant cannot read stored periods. Whatever rows you give these
  functions still go to the vendor.
- **Review `describe`.** `docker run --rm IMAGE` prints every tool the
  assistant can call, with its input and output schema. Turn off tools you do
  not want in the client's per-server tool settings.
- **Keep raw inputs out of traces.** Leave `BOXSCORE_TRACE_INPUTS` unset
  (the default) so local traces keep hashes only. This limits what is stored
  on your side; it does not change what the client sends.
- **Use neutral labels.** Property labels and account names are echoed in
  results; a code such as `synthetic_ops` reveals less than a street address.
- **Use the vendor plan your policy requires** (for example one with zero
  data retention or no training on your data). That is a contract decision
  outside this software.

## 6. How to back up

The database is one file. Copy it while no container is using it: quit
Claude Desktop, stop any `serve` container and let any `docker run` finish,
then copy it out through a
created-but-not-started container with `docker cp`. Restore by copying it into
a new volume as a tar stream owned by uid 65532. Commands are in
[INSTALL.md](INSTALL.md#back-up-and-restore). [CI: backs up with `docker cp`,
restores into a fresh volume, and checks that `review` of the restored copy is
byte-identical to the original]

For an online backup while writers may be active, keep the database in a host
directory and use SQLite's backup API from the host:
`sqlite3 /srv/boxscore/exact.sqlite ".backup backup.sqlite"`. Do not copy the
file while a write is in progress. [stated]

Backups contain all stored financial data. Treat them like the original.

## 7. How to delete

- `docker volume rm boxscore-data` deletes the database, its invocation
  trace, and an `invocations.jsonl` if `BOXSCORE_TRACE_DIR` pointed at
  `/data`. Removing a volume does not securely erase the disk blocks;
  full-disk encryption on the host is the control for that.
- Delete your backups and the export files you mounted, which the container
  never modifies, and an `invocations.jsonl` in any other trace directory you
  configured. Delete an API key file you no longer use.
- `docker image rm` removes the image; it contains no user data. [CI]
- If you ran a container detached, `docker rm` it to delete its Docker log.
- Conversations with an AI assistant, including tool results, are stored by
  the client and the vendor. Delete them there, under the vendor's retention
  terms.

## 8. How to verify the image

- **Digest.** Release images are tagged `<version>` and `sha-<7 hex>`, both
  pointing at one digest; there is no `latest`. `docker buildx imagetools
  inspect ghcr.io/paintbrushv/plat-operations:<version>` prints it. After
  verifying, refer to the image by digest. [CI: the publish job checks both
  tags resolve to the same digest]
- **Signature.** The release workflow signs the digest with cosign keyless
  signing (Sigstore, GitHub OIDC). Verify that it was signed by this
  repository's image workflow for that tag:

  ```bash
  cosign verify \
    --certificate-identity "https://github.com/paintbrushv/plat-operations/.github/workflows/image.yml@refs/tags/v<version>" \
    --certificate-oidc-issuer https://token.actions.githubusercontent.com \
    ghcr.io/paintbrushv/plat-operations@sha256:<digest>
  ```

  [CI: the publish job runs this verification after signing]
- **SBOM.** Every image build produces two SPDX JSON SBOMs, uploaded as the
  `boxscore-exact-sbom` workflow artifact: one of the image (the distroless
  Debian packages and the binary) and one of the Rust lockfile
  (`boxscore/Cargo.lock`). The image SBOM is also attached to each release
  image as a signed attestation:

  ```bash
  cosign verify-attestation --type spdxjson \
    --certificate-identity "https://github.com/paintbrushv/plat-operations/.github/workflows/image.yml@refs/tags/v<version>" \
    --certificate-oidc-issuer https://token.actions.githubusercontent.com \
    ghcr.io/paintbrushv/plat-operations@sha256:<digest>
  ```

  The binary does not embed its crate list, so the image SBOM does not
  enumerate Rust crates. The lockfile SBOM does. It is a superset of what the
  binary contains: it also lists crates used only by the legacy `boxscore`
  program and by tests. [CI generates both]
- **Provenance labels.** `org.opencontainers.image.revision` is the git
  commit the image was built from and `org.opencontainers.image.version` the
  release version. [CI checks both against the build] You can rebuild from
  that commit with the root `Dockerfile` and run `scripts/check_image.py`
  against your build. The build is not bit-for-bit reproducible, so compare
  behaviour, not digests.
- **What a signature does not prove.** It proves the image came from this
  repository's workflow at that tag. It does not prove the code is free of
  defects or that your inputs are right.

## 9. Base images and hardening

- Build stage: `rust:1.99-slim-bookworm`. Runtime: `gcr.io/distroless/cc-debian12:nonroot`
  (glibc and libgcc only; no shell, no package manager). Both are pinned by
  digest in the `Dockerfile` and re-resolved when a release is cut. [stated]
- The build context is an allowlist (`.dockerignore`): the crate manifest,
  lockfile, Rust sources and embedded SQL migrations. Local databases, `.env`
  files, sample data and git metadata cannot enter the image. [CI checks that
  no database or data file is in the image]
- The binary is owned by root and not writable by the container user. [CI]
- No shell: `docker run --entrypoint /bin/sh IMAGE` fails. [CI]
- Recommended runtime flags, used by every CI check: `--network none
  --read-only --cap-drop ALL --security-opt no-new-privileges`. The `serve`
  check is the one exception to `--network none`: it uses the default bridge
  network and publishes the port to `127.0.0.1` only. [CI]
- Published for `linux/amd64` only.
