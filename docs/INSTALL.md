# Install and run with Docker

The `boxscore-exact` image runs the exact-cent calculation catalog without a
Rust or Python toolchain. It contains one binary on a distroless base: no
shell, no package manager, runs as a non-root user (uid 65532), and listens on
no port unless you start [`serve`](#http-server-serve). [DATA_FLOW.md](DATA_FLOW.md) describes what goes in, what is stored
and what leaves, for an IT review. The commands below are the same ones
[EXACT_CENTS.md](EXACT_CENTS.md) documents for the native binary.

Examples use version `0.2.0`. Use the release you reviewed, and pin it by
digest once you have verified it (see [Verify the image](#verify-the-image)).
The image is published for `linux/amd64`. On Apple Silicon, Docker Desktop
runs it under emulation; add `--platform linux/amd64` to silence the warning.

```bash
IMAGE=ghcr.io/paintbrushv/plat-operations:0.2.0
docker pull "$IMAGE"
```

> **Published image.** Release 0.2.0 is the first version pushed to GHCR, as
> `ghcr.io/paintbrushv/plat-operations:0.2.0` (`linux/amd64`; the same digest is
> also tagged `sha-<first 7 hex of the commit>`, and there is no `latest` tag).
> The release workflow signs each image with cosign keyless signing and attaches
> its SBOM as a signed attestation. Verify both before first use; the commands
> are in [Verify the image](#verify-the-image).
>
> To build the image yourself instead, skip `docker pull` and run this from the
> repository root (the command is the one in the Dockerfile header, and
> `scripts/check_image.py` builds and checks the image the same way):
>
> ```bash
> docker build --build-arg VERSION=0.2.0 --build-arg REVISION="$(git rev-parse HEAD)" -t boxscore-exact:local .
> IMAGE=boxscore-exact:local
> ```

## Recommended flags

Every check the CI image job runs uses these flags, except that the `serve`
check needs a network in place of `--network none` (see
[HTTP server](#http-server-serve)). Nothing else the image does needs more:

```bash
SAFE="--network none --read-only --cap-drop ALL --security-opt no-new-privileges"
```

- `--network none`: the container has no network interface. The binary makes
  no network connection, so nothing breaks. Only `serve`, which accepts
  connections (and, with OAuth configured, calls the identity provider's
  introspection endpoint), needs a network interface.
- `--read-only`: the root filesystem is read-only. The only writable place is
  the `/data` volume you mount.
- `--cap-drop ALL`, `--security-opt no-new-privileges`: no Linux capabilities
  and no privilege escalation.

## One-liners

Describe the catalog. This is the default command; it is stateless and needs
no volume. It prints one JSON envelope with `"status":"calculated"`:

```bash
docker run --rm $SAFE "$IMAGE"
# same as: docker run --rm $SAFE "$IMAGE" call --function describe
```

Create the database once, in a named volume:

```bash
docker run --rm $SAFE -v boxscore-data:/data "$IMAGE" init --database /data/exact.sqlite
```

Import one period from canonical CSV exports (headers
`account_code,account_name,category,amount`; see
[EXACT_CENTS.md](EXACT_CENTS.md#build-and-run)). Mount the folder that holds
them read-only at `/in`:

```bash
docker run --rm $SAFE -v boxscore-data:/data -v "$PWD/exports:/in:ro" "$IMAGE" \
  import-csv --database /data/exact.sqlite \
  --actuals /in/actuals.csv --budgets /in/budgets.csv \
  --property synthetic_ops --period 2026-05 --units 10
# {"contract_version":"plat.ops/1","revision_id":"REVISION_ID","status":"imported"}
```

On Linux the container user (uid 65532) must be able to read the exports.
Grant that one user read access rather than making the files world-readable,
for example `setfacl -R -m u:65532:rX exports`. Docker Desktop on macOS and
Windows shares host files with your own account's access, so usually no change
is needed there.

Review the revision (`status` is `calculated`, or `review_required` with the
reasons listed):

```bash
docker run --rm $SAFE -v boxscore-data:/data:ro "$IMAGE" \
  review --database /data/exact.sqlite --revision REVISION_ID
```

Issue an immutable report record, and list revisions:

```bash
docker run --rm $SAFE -v boxscore-data:/data "$IMAGE" \
  issue --database /data/exact.sqlite --revision REVISION_ID
echo '{}' | docker run --rm -i $SAFE -v boxscore-data:/data:ro "$IMAGE" \
  call --database /data/exact.sqlite --function list_revisions
```

Any catalog function runs the same way: input JSON on stdin (`-i`), the
provenance envelope on stdout. `call` exits 2 on a refusal.

Each catalog call is traced (see
[DATA_FLOW.md](DATA_FLOW.md#2-where-it-is-stored) and
[PROVENANCE.md](PROVENANCE.md)). With `--database` the record goes into the
database. Without one, nothing is recorded unless you pass
`-e BOXSCORE_TRACE_DIR=/data` (with a volume on `/data`), which appends to
`/data/invocations.jsonl`. Add `-e BOXSCORE_TRACE_INPUTS=1` only if the trace
should also keep the raw input, your numbers included; by default it keeps
hashes only. Both options work the same way on `mcp` and `serve`.

## MCP over stdio (Claude Desktop)

`mcp` serves the catalog as an MCP server over stdin and stdout, so Docker
needs `-i` and nothing else: no port is published. Initialize the database
first (above), then add this to `claude_desktop_config.json` (macOS:
`~/Library/Application Support/Claude/claude_desktop_config.json`, Windows:
`%APPDATA%\Claude\claude_desktop_config.json`):

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
        "ghcr.io/paintbrushv/plat-operations:0.2.0",
        "mcp", "--database", "/data/exact.sqlite"
      ]
    }
  }
}
```

The minimal form is `docker run -i --rm -v boxscore-data:/data
ghcr.io/paintbrushv/plat-operations:0.2.0 mcp --database /data/exact.sqlite`;
the extra flags only take privileges away. If Claude Desktop cannot find
`docker`, give its absolute path in `command` (for example
`/usr/local/bin/docker`). Restart Claude Desktop. Each session starts a fresh
container, which is removed when the session ends; the database stays in the
volume. To run stateless (only `describe`, `noi_variance`, and `occupancy`,
`economic_occupancy` and `delinquency` on an inline snapshot; the stateful
tools refuse with `DATABASE_REQUIRED`), drop the `-v` pair and
`"--database", "/data/exact.sqlite"`. The tools, the grounding rule and the
error codes are described in
[connectors/claude-desktop.md](connectors/claude-desktop.md).

Before connecting an agent client, read
[what the client sends to the model vendor](DATA_FLOW.md#5-what-the-agent-client-sends-to-the-model-vendor).

## HTTP server (`serve`)

`serve` runs the catalog as an HTTP server: MCP Streamable HTTP at `/mcp`,
`POST /v1/calc/{function}`, `/openapi.json`, `/health` and the OAuth
protected-resource metadata. It is not the default command, and nothing
listens until you start it. Routes, flags and API key rules are in
[connectors/remote-mcp.md](connectors/remote-mcp.md).

Inside a container the server must bind `0.0.0.0`, and it refuses a
non-loopback address without an API key file or OAuth configuration (see
[With OAuth](#with-oauth-claudeai-and-chatgpt-connectors) below). Create the
key file first, with
mode 0600. On Linux, make it readable by the container user (uid 65532) and
nobody else:

```bash
(umask 077; printf 'ops-script:%s\n' "$(openssl rand -hex 32)" > boxscore-keys)
chmod 600 boxscore-keys
cat boxscore-keys                       # copy the secret (after the colon) for your scripts now
sudo chown 65532:65532 boxscore-keys    # afterwards only root and uid 65532 can read it
```

A key file that other users can read or write makes `serve` refuse to start;
group access prints a warning. Then start the server with the key file
mounted read-only and the port published to the host loopback only:

```bash
docker run -d --rm --name boxscore-serve \
  --read-only --cap-drop ALL --security-opt no-new-privileges \
  -p 127.0.0.1:3819:3819 \
  -v boxscore-data:/data \
  -v "$PWD/boxscore-keys:/run/secrets/boxscore-keys:ro" \
  "$IMAGE" serve --bind 0.0.0.0:3819 --database /data/exact.sqlite \
  --api-key-file /run/secrets/boxscore-keys
```

- `-p 127.0.0.1:3819:3819` publishes the port on the host's loopback
  interface only. Do not publish it on all interfaces (`-p 3819:3819`); for
  access from other machines, put a TLS reverse proxy in front and pass
  `--public-url https://...` (see
  [remote-mcp.md](connectors/remote-mcp.md#tls)).
- `--network none` cannot be used here: the server needs a network interface
  to accept connections. With API keys only, it still opens no outbound
  connection.
- Drop `--database /data/exact.sqlite` (and the volume) to serve only the
  stateless functions.
- Add `-e BOXSCORE_TRACE_DIR=/data` (useful only without `--database`) or
  `-e BOXSCORE_TRACE_INPUTS=1` as described under [One-liners](#one-liners).
- `docker stop boxscore-serve` stops it gracefully (exit code 0). Its Docker
  log holds only the start-up and shutdown lines; requests, tokens and
  results are not logged.

Check that it is up, then call it with the key:

```bash
curl http://127.0.0.1:3819/health
# {"producer":{...},"status":"ok"}
curl -s -X POST http://127.0.0.1:3819/v1/calc/describe -H "Authorization: Bearer $BOXSCORE_KEY"
```

The CI image job runs the same `serve` command with a synthetic key file and
checks `/health`, the metadata route, the workbench page at `/`, the `401`
challenge without a token, an MCP `initialize` with it, and that
`/v1/calc/describe` answers the server's own origin and refuses another.
Before connecting a remote assistant, read
[what the client sends to the model vendor](DATA_FLOW.md#5-what-the-agent-client-sends-to-the-model-vendor).

### Browser workbench

`serve` also offers a small page for analysts without an agent
connection. Open the server's public URL with a trailing slash in a browser
(`http://127.0.0.1:3819/` with the command above), paste an API key when the
page asks for one (it is kept in that tab's session storage only), and pick
a revision from the list to see its review: status and review reasons, the
NOI bridge, every account, excluded and unmapped accounts, and the citation
line with a Copy button. A loopback server started without a key file asks
for no key.

Two more screens sit around the list. **Load a period** is built from the
server's own list of source adapters (`describe`): choose a profile, choose
a file for each file it names (required ones are marked, with the expected
shape beside each), and fill in its options (for `canonical.csv/1`: the
property name, the period and the unit count). The page reads the bytes of
each file you choose and decodes them as strict UTF-8, keeping a leading
byte order mark if the file has one, and sends that text to the server in
the body of the request, so the server hashes exactly the bytes on disk (its
CSV reader skips a BOM; a JSON file with one, a snapshot say, is refused, by
the page's import and by `import-with` alike). A file that is not UTF-8 is
refused in the page with "<name> is not UTF-8 text. Nothing was sent." The
page keeps no copy, and it never sees a file's path. The server runs the
adapter, stores a revision, and the page then shows the new revision id, the
citation line and the recorded files (name, size and SHA-256) and opens the
new revision in the review. The page itself refuses only a missing required
file or option, an option with leading or trailing spaces, a correction with
no reason, and a request over the server's cap: about 2 MiB of JSON
including escaping (each quote, backslash or line break in a file takes two
bytes), checked first on the files' sizes and again on the request it built,
before sending. Every other refusal is the engine's, shown with its code and
message as it sent them. An import waits up to 60 seconds for an answer; if
none comes, the server may already have stored the period, so reload the
list before trying again. Tick **This is a correction** to supersede a
revision (the id of the one under review is filled in) with a reason: the
page then shows the property, period, the revision it supersedes and the
reason, and sends nothing until you press **Confirm correction**. **Look up
a trace** takes a trace id from any citation line (the Look up button beside
a citation fills it in) and shows what the server recorded for that call:
function and version, caller, status, input and output hashes, times, and
the revision's recorded sources (the server's file list, up to 50 rows, and
anything the caller sent under `sources.supplied`, shown apart as not
verified by the server).

Below the review, **Issued reports** lists the reports already issued for
that revision (report id, when, and the report's SHA-256, shortened with the
whole value on hover), or says none has been. **Issue report** first shows
what you are about to issue: the property, the period, the revision number and
id, the review's status and its review reasons. It says that an issued report
is an immutable record that is not sent anywhere, and that issuing is a
record, not sign-off. Nothing is sent until you press **Confirm issue**;
**Cancel** sends nothing. The engine decides whether a report can be issued
(the page does not second-guess a `review_required` review, and the engine
does not refuse to issue one), and its refusal is shown with its code and
message as it sent them; the button is off only when the review itself was
refused. When the loaded list shows a later revision that supersedes the
one under review, the confirmation says so (and so does the Issued reports
block) and that the engine will refuse; if you confirm anyway, the engine
refuses with `SUPERSEDED`, naming the latest revision, and the page shows
that refusal. Issue the latest revision instead. If the request fails without an answer (a timeout, a
network failure or a server error), the report may already exist, so
reload the list before trying again. Issuing the same revision again makes another report with its own id
and hash and never changes the first. The page then shows the new report id,
the hash and the citation line, whose Look up button opens the trace, with
its Report section.

On a shared deployment with OAuth, the page can sign analysts in through
the firm's identity provider instead: IT registers a public client for the
page (authorization code with PKCE `S256`, redirect URI `<public-url>/`, the
page's origin allowed on the token endpoint, the `boxscore:calc` scope with
the server's audience) and starts `serve` with
`--oauth-browser-client-id <id>` next to the OAuth flags. The page then shows
**Sign in with your firm's identity provider**, goes to the IdP's sign-in
page and back, and keeps the access token in that tab's session storage
until it expires (the next request after that is not sent and the page asks
to sign in again; no refresh token is kept). Open the page at the redirect
URI's origin exactly (`127.0.0.1` and `localhost` are different origins),
and give it an origin no other web app shares. **Sign out** forgets it in
that tab and does not end the IdP session. When
the server also has a key file, the key form stays available beside it. The
steps, what goes where, and a verified Keycloak run are in
[remote-mcp.md, Browser workbench sign-in](connectors/remote-mcp.md#browser-workbench-sign-in).
The page is served from the binary on the same port, loads nothing from
elsewhere, and is not part of `/openapi.json`. Open it at the URL the
server prints (`workbench at ...`): the API answers the page only from the
server's own origin, unless you add another with `--allow-origin`.

The public URL must be the URL the browser uses. The server knows its own
origin only from `--public-url` (default `http://127.0.0.1:<port>`, the
container's port) and, for a loopback bind, the bound port; it cannot see
how Docker or a proxy remapped it. Publishing the container on another host
port, for example `-p 127.0.0.1:9000:3819` with `--bind 0.0.0.0:3819`, makes
the browser send `Origin: http://127.0.0.1:9000`, which is refused with
`403 ORIGIN_NOT_ALLOWED`. Pass `--public-url http://127.0.0.1:9000` (or
`--allow-origin http://127.0.0.1:9000`) in that case, or publish on the same
port as the command above does. The banner's `workbench at` line is built
from the public URL, so it is the right address only when the public URL is.
Behind a reverse proxy with a path prefix (`--public-url
https://host/boxscore`), open the page with its trailing slash
(`https://host/boxscore/`); it loads its files and calls the API relative to
that address.

### With OAuth (claude.ai and ChatGPT connectors)

Claude.ai and ChatGPT connectors sign users in through your identity provider
(IdP) and send OAuth access tokens, which `serve` checks with the IdP's token
introspection endpoint. Set up the IdP first (the checklist, and which IdPs
work, are in [remote-mcp.md](connectors/remote-mcp.md#oauth-21-claudeai-and-chatgpt-connectors)).
Put the secret of the client this server uses for introspection in a 0600
file owned by the container user, as for the key file:

```bash
(umask 077; printf '%s\n' "$INTROSPECTION_CLIENT_SECRET" > boxscore-oauth-secret)
sudo chown 65532:65532 boxscore-oauth-secret
```

```bash
docker run -d --rm --name boxscore-serve \
  --read-only --cap-drop ALL --security-opt no-new-privileges \
  -p 127.0.0.1:3819:3819 \
  -v boxscore-data:/data \
  -v "$PWD/boxscore-keys:/run/secrets/boxscore-keys:ro" \
  -v "$PWD/boxscore-oauth-secret:/run/secrets/boxscore-oauth-secret:ro" \
  "$IMAGE" serve --bind 0.0.0.0:3819 --database /data/exact.sqlite \
  --public-url https://boxscore.example.com \
  --api-key-file /run/secrets/boxscore-keys \
  --authorization-server https://idp.example.com/oauth2/default \
  --introspection-endpoint https://idp.example.com/oauth2/default/v1/introspect \
  --oauth-client-id boxscore-resource-server \
  --oauth-client-secret-file /run/secrets/boxscore-oauth-secret \
  --oauth-audience https://boxscore.example.com/mcp
```

- `--api-key-file` is optional here; keep it if scripts also call the
  server. API keys are checked first, without calling the IdP.
- `--introspection-endpoint` may be dropped: the server then reads it once
  at start-up from the issuer's RFC 8414 metadata (for an issuer with a
  path, `https://host/.well-known/oauth-authorization-server/<path>` first,
  then `<issuer>/.well-known/oauth-authorization-server` on a `404`) and
  refuses to start if the metadata has none.
- The container now makes outbound HTTPS calls to the introspection
  endpoint, so it needs a network with a route (and DNS) to the IdP;
  `--network none` is incompatible with OAuth. See
  [DATA_FLOW.md](DATA_FLOW.md#4-what-leaves-the-host) for exactly what is
  sent.
- The IdP must have a certificate from a public certificate authority: the
  binary trusts the Mozilla root set built into it, not the host's store.
- The Docker log gains one line naming the issuer, introspection endpoint,
  audience and scope, and one line per failed introspection call. Tokens and
  the client secret are never logged.

This container recipe has not been run against a real IdP. The native
binary was run against Keycloak 26.0 on one machine (the
[Keycloak recipe](connectors/remote-mcp.md#local-test-with-keycloak-verified-against-keycloak-260-on-2026-10-12)), and the server
side is covered by `boxscore/tests/exact_mcp_http_oauth.rs` with a mock IdP.

## Volume layout

| path in the container | what | owner, mode |
|---|---|---|
| `/data` | the named volume (`boxscore-data` above); also the working directory | uid 65532, `0700` |
| `/data/exact.sqlite` | the database: every revision, snapshot and issued report | uid 65532, `0600` |
| `/data/exact.sqlite-journal` | SQLite rollback journal, present only while a write is in progress | uid 65532, `0600` |
| `/data/invocations.jsonl` | the JSONL invocation trace, only with `-e BOXSCORE_TRACE_DIR=/data` and no `--database` | uid 65532, `0600` |
| `/in` (your bind mount, read-only) | the exports you import; not part of the image | yours |
| `/run/secrets/boxscore-keys` (your bind mount, read-only) | API keys for `serve`; not part of the image | uid 65532, `0600` |

The image ships `/data` empty. A new named volume copies its owner and mode,
which is why the non-root process can create the database there. To use a host
directory instead of a named volume, create it with owner 65532 and mode 0700
(`sudo install -d -o 65532 -g 65532 -m 0700 /srv/boxscore`) and mount it with
`-v /srv/boxscore:/data`.

## Back up and restore

Stop every container using the volume first (quit Claude Desktop,
`docker stop boxscore-serve`, let any `docker run` finish). Then copy the database out through a container that
is created but never started:

```bash
docker container create --name boxscore-backup -v boxscore-data:/data "$IMAGE"
docker cp boxscore-backup:/data/exact.sqlite "./exact-$(date +%F).sqlite"
docker rm boxscore-backup
```

To restore into a new volume, the file must be owned by uid 65532 and keep
mode 0600. Copy it in as a tar stream with that owner:

```bash
docker volume create boxscore-restored
docker container create --name boxscore-restore -v boxscore-restored:/data "$IMAGE"
cp exact-2026-10-11.sqlite exact.sqlite && chmod 600 exact.sqlite
tar --numeric-owner --owner=65532 --group=65532 -cf - exact.sqlite | docker cp -a - boxscore-restore:/data
# macOS (bsdtar): tar --uid 65532 --gid 65532 -cf - exact.sqlite | docker cp -a - boxscore-restore:/data
docker rm boxscore-restore
```

The CI image job runs this backup and restore and checks that `review` of the
restored copy is byte-identical to the original. For an online backup without
stopping writers, keep the database in a host directory and use the host's
`sqlite3 /srv/boxscore/exact.sqlite ".backup backup.sqlite"`.

## Upgrade and rollback

Images are tagged by version (`0.2.0`) and by commit (`sha-<7 hex>`); there is
no `latest` tag. To upgrade, back up, pull the new tag, and change the tag in
your commands and in `claude_desktop_config.json`. To roll back, put the old
tag back.

The database records its schema version. When a release changes it, the new
image refuses an older database with `SCHEMA_MISMATCH` instead of altering it,
and an older image refuses a newer database the same way; the release notes
say how to move the data. Keep the backup you took before upgrading until you
no longer need to roll back.

## Health check

For one-shot commands and the stdio MCP server nothing listens, so the health
check is the stateless describe call. It exits 0 and prints
`"status":"calculated"`:

```bash
docker run --rm $SAFE "$IMAGE" call --function describe
```

For a running `serve` container, probe `/health` from the host; it needs no
token and answers `200` with `"status":"ok"`:

```bash
curl http://127.0.0.1:3819/health
```

The image defines no `HEALTHCHECK`: the default command does not listen, and
the image has no shell or `curl` to probe with.

## Verify the image

Each release image is signed with cosign keyless signing from the release
workflow, and its SBOM is attached as a signed attestation. Check both before
first use:

```bash
docker buildx imagetools inspect ghcr.io/paintbrushv/plat-operations:0.2.0   # shows the digest
cosign verify \
  --certificate-identity "https://github.com/paintbrushv/plat-operations/.github/workflows/image.yml@refs/tags/v0.2.0" \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  ghcr.io/paintbrushv/plat-operations:0.2.0
cosign verify-attestation --type spdxjson \
  --certificate-identity "https://github.com/paintbrushv/plat-operations/.github/workflows/image.yml@refs/tags/v0.2.0" \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  ghcr.io/paintbrushv/plat-operations:0.2.0
```

Then refer to the image by digest
(`ghcr.io/paintbrushv/plat-operations@sha256:...`) in your commands and
configuration. [DATA_FLOW.md](DATA_FLOW.md#8-how-to-verify-the-image) covers
the SBOM and what the signature does and does not prove.

## Build it yourself

From a checkout of the repository:

```bash
docker build --build-arg VERSION=0.2.0 --build-arg REVISION="$(git rev-parse HEAD)" -t boxscore-exact:local .
python3 scripts/check_image.py boxscore-exact:local --expect-version 0.2.0
```

`scripts/check_image.py` is the same check the CI image job runs.

## Remove everything

```bash
docker volume rm boxscore-data        # deletes the database
docker image rm "$IMAGE"
```

See [DATA_FLOW.md](DATA_FLOW.md#7-how-to-delete) for copies that live outside
the volume.
