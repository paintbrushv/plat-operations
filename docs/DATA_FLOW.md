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
| Does it listen on a port? | No. No `EXPOSE`; no subcommand opens a socket. | [CI] runs with `--network none`; [stated] |
| Does it connect out? | No. No telemetry, no update check, no crash reporting. | [CI] runs with `--network none`; [stated] |
| Where is data stored? | One SQLite file you name, on the `/data` volume, mode 0600. | [CI] |
| What else does it write? | Nothing. The root filesystem can be read-only. | [CI] runs with `--read-only` |
| Does it run as root? | No: uid/gid 65532. No shell in the image. | [CI] |
| Is data baked into the image? | No. `/data` ships empty; no database or data file is in the image. | [CI] |
| Does anything reach an AI vendor? | Not from the container. If you connect an agent client (Claude Desktop), **the client sends tool arguments and results, including your numbers, to the model vendor.** See section 5. | [stated] |

## 1. What enters

| input | how | limit |
|---|---|---|
| Canonical GL exports: CSV with `account_code,account_name,category,amount` | a file path on a mount you choose (`import-csv`) | 2 MiB per file |
| A canonical period as JSON, an optional occupancy snapshot JSON | a file path (`import --input`, `import-csv --snapshot`) | 2 MiB per file |
| A legacy database and a signed-off review file for reviewed migration | file paths (`plan-migration`, `migrate`) | review file 2 MiB |
| A calculation request (`call`, `protocol`) | JSON on stdin | 2 MiB |
| MCP tool calls from an agent client (`mcp`) | JSON-RPC on stdin, one message per line | 2 MiB per line |
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

The only environment variable the program reads is `BOXSCORE_MCP_DEBUG`
(section 4). It does not load `.env` files. [stated:
`boxscore/src/bin/boxscore-exact.rs` `main`, `boxscore/src/exact/mcp.rs`]

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
  `archived_reports` (reviewed migration records). [stated:
  `boxscore/src/exact/store.rs`]
- **Append-only.** Every one of those tables has `BEFORE UPDATE` and
  `BEFORE DELETE` triggers that abort with `immutable exact record`.
  Corrections add a new revision; they never change an old one. [tests:
  `boxscore/tests/exact_store.rs` attempts an UPDATE and a DELETE and expects
  refusal] These triggers and the content hashes are integrity checks, not
  access control: anyone who can write the file can drop a trigger. Protect
  the volume with host permissions, disk encryption and backups.

**Migration output.** `migrate --destination DIR` writes a new directory
holding a byte-identical read-only copy of the legacy database, a new
`exact.sqlite` and `migration-report.json`. It writes only where you point it.
[stated: `boxscore/src/exact/migration.rs`]

**Invocation trace.** This image version writes no trace or log file. A
planned provenance change (not in this image) adds an append-only
`exact_invocations` table in the same database, or, when no database is in
use and `BOXSCORE_TRACE_DIR` names a directory, an `invocations.jsonl` file
with mode 0600 there. In that design the trace keeps hashes only; the input
itself is retained only if the operator sets `BOXSCORE_TRACE_INPUTS=1`. This
page will be updated when that ships.

**Output streams.** Results go to stdout as JSON. Diagnostics go to stderr
only when `BOXSCORE_MCP_DEBUG` is set, and they name MCP methods, never
arguments or results. [stated: `boxscore/src/exact/mcp.rs`] Docker itself
keeps the stdout of a **detached** container (`docker run -d`) in its log
driver, which would include results. The documented commands run attached
with `--rm`, so output goes only to your terminal or to the MCP client.

## 3. What is computed

Deterministic arithmetic in checked signed 64-bit integer cents: account-level
actual versus budget variance, the NOI bridge (revenue, expenses, NOI and
their variances), review reasons (for example a missing occupancy snapshot,
accounts with only an actual or only a budget, unmapped accounts or negative
net expenses), correction bridges between revisions, and
immutable report records. Every result carries `function@version`,
`input_sha256` and `output_sha256`, so a number can be reproduced from its
citation. Every review and report says `human_approval: not_granted`.

The container runs no model, no forecasting and no inference. It does not
contact an AI service. [stated: `boxscore/src/exact/`; [tests] cover the
arithmetic, overflow refusal and immutability]

## 4. What leaves the host

From the container itself, nothing.

**Listeners: none.** The image declares no `EXPOSE`. No subcommand in this
version opens a socket. `mcp` speaks only over stdin and stdout of the
process that started it. [CI: every functional check, including the MCP
session, runs with `--network none`, so none of them needs a network;
stated: no socket, HTTP or network client code is reachable from
`boxscore/src/bin/boxscore-exact.rs` and `boxscore/src/exact/`]

A planned `serve` command (separate change, not in this image) will add an
HTTP MCP endpoint that listens only when started explicitly with `serve`,
requires an API key file and binds loopback by default. In a container it
must bind inside the container and be published only to the host loopback
(`-p 127.0.0.1:PORT:PORT`). This page will be updated when that ships.

**Outbound connections: none.** No telemetry, no analytics, no update check,
no crash reporting, no DNS lookups, no licence server. [CI: all checks pass
with `--network none`; stated: as above] Run with `--network none` (as the
[INSTALL.md](INSTALL.md) examples do) and the kernel enforces it.

**Files written: only the database** (and its transient journal), plus the
migration directory if you run `migrate`. [CI: the checks run with
`--read-only`, so any other write would fail, and the volume holds only
`exact.sqlite` afterwards]

The Docker daemon contacts `ghcr.io` when you pull the image. That is
Docker's traffic, not the container's.

## 5. What the agent client sends to the model vendor

**Read this before connecting the MCP server to an AI assistant.**

When `boxscore-exact mcp` is connected to Claude Desktop (or, once `serve`
exists, to ChatGPT or another remote client), the assistant decides which
tool to call and with what arguments. The client application sends **the tool
call arguments and the full tool result** to the model vendor (Anthropic for
Claude, OpenAI for ChatGPT) as part of the conversation. That includes your
numbers: GL rows passed to `noi_variance` or `import_period`, review results
with account names, amounts and variances, and revision lists. It also
includes anything you type or paste into the chat. The container does not
send this; the client does, under your organisation's agreement with that
vendor (data retention, training use, region). The container cannot see or
limit it. [stated]

How to limit what reaches the vendor:

- **Import outside the chat.** Run `import-csv` from the command line
  (see the one-liners in [INSTALL.md](INSTALL.md)), so raw exports never pass
  through the assistant. Results the assistant asks for, such as a review, still go
  to the vendor.
- **Run stateless when you can.** Start `mcp` without `--database`: only
  `describe` and `noi_variance` work, nothing is stored, and the assistant
  cannot read stored periods. Whatever rows you give `noi_variance` still go
  to the vendor.
- **Review `describe`.** `docker run --rm IMAGE` prints every tool the
  assistant can call, with its input and output schema. Turn off tools you do
  not want in the client's per-server tool settings.
- **Keep raw inputs out of traces.** When the provenance trace ships, leave
  `BOXSCORE_TRACE_INPUTS` unset so local traces keep hashes only.
- **Use neutral labels.** Property labels and account names are echoed in
  results; a code such as `synthetic_ops` reveals less than a street address.
- **Use the vendor plan your policy requires** (for example one with zero
  data retention or no training on your data). That is a contract decision
  outside this software.

## 6. How to back up

The database is one file. Copy it while no container is using it: quit
Claude Desktop and let any `docker run` finish, then copy it out through a
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

- `docker volume rm boxscore-data` deletes the database. Removing a volume does
  not securely erase the disk blocks; full-disk encryption on the host is the
  control for that.
- Delete your backups and the export files you mounted, which the container
  never modifies.
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
  --read-only --cap-drop ALL --security-opt no-new-privileges`. [CI]
- Published for `linux/amd64` only.
