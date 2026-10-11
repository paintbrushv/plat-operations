# Install and run with Docker

The `boxscore-exact` image runs the exact-cent calculation catalog without a
Rust or Python toolchain. It contains one binary on a distroless base: no
shell, no package manager, runs as a non-root user (uid 65532), and listens on
no port. [DATA_FLOW.md](DATA_FLOW.md) describes what goes in, what is stored
and what leaves, for an IT review. The commands below are the same ones
[EXACT_CENTS.md](EXACT_CENTS.md) documents for the native binary.

Examples use version `0.1.1`. Use the release you reviewed, and pin it by
digest once you have verified it (see [Verify the image](#verify-the-image)).
The image is published for `linux/amd64`. On Apple Silicon, Docker Desktop
runs it under emulation; add `--platform linux/amd64` to silence the warning.

```bash
IMAGE=ghcr.io/paintbrushv/plat-operations:0.1.1
docker pull "$IMAGE"
```

## Recommended flags

Every check the CI image job runs uses these flags, and nothing the image does
needs more:

```bash
SAFE="--network none --read-only --cap-drop ALL --security-opt no-new-privileges"
```

- `--network none`: the container has no network interface. The binary makes
  no network connection, so nothing breaks.
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
        "ghcr.io/paintbrushv/plat-operations:0.1.1",
        "mcp", "--database", "/data/exact.sqlite"
      ]
    }
  }
}
```

The minimal form is `docker run -i --rm -v boxscore-data:/data
ghcr.io/paintbrushv/plat-operations:0.1.1 mcp --database /data/exact.sqlite`;
the extra flags only take privileges away. If Claude Desktop cannot find
`docker`, give its absolute path in `command` (for example
`/usr/local/bin/docker`). Restart Claude Desktop. Each session starts a fresh
container, which is removed when the session ends; the database stays in the
volume. To run stateless (only `describe` and `noi_variance`; the stateful
tools refuse with `DATABASE_REQUIRED`), drop the `-v` pair and
`"--database", "/data/exact.sqlite"`. The tools, the grounding rule and the
error codes are described in
[connectors/claude-desktop.md](connectors/claude-desktop.md).

Before connecting an agent client, read
[what the client sends to the model vendor](DATA_FLOW.md#5-what-the-agent-client-sends-to-the-model-vendor).

## Volume layout

| path in the container | what | owner, mode |
|---|---|---|
| `/data` | the named volume (`boxscore-data` above); also the working directory | uid 65532, `0700` |
| `/data/exact.sqlite` | the database: every revision, snapshot and issued report | uid 65532, `0600` |
| `/data/exact.sqlite-journal` | SQLite rollback journal, present only while a write is in progress | uid 65532, `0600` |
| `/in` (your bind mount, read-only) | the exports you import; not part of the image | yours |

The image ships `/data` empty. A new named volume copies its owner and mode,
which is why the non-root process can create the database there. To use a host
directory instead of a named volume, create it with owner 65532 and mode 0700
(`sudo install -d -o 65532 -g 65532 -m 0700 /srv/boxscore`) and mount it with
`-v /srv/boxscore:/data`.

## Back up and restore

Stop every container using the volume first (quit Claude Desktop, let any
`docker run` finish). Then copy the database out through a container that
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

Images are tagged by version (`0.1.1`) and by commit (`sha-<7 hex>`); there is
no `latest` tag. To upgrade, back up, pull the new tag, and change the tag in
your commands and in `claude_desktop_config.json`. To roll back, put the old
tag back.

The database records its schema version. When a release changes it, the new
image refuses an older database with `SCHEMA_MISMATCH` instead of altering it,
and an older image refuses a newer database the same way; the release notes
say how to move the data. Keep the backup you took before upgrading until you
no longer need to roll back.

## Health check

Nothing listens until the planned `serve` command exists, so the health check
is the stateless describe call. It exits 0 and prints `"status":"calculated"`:

```bash
docker run --rm $SAFE "$IMAGE" call --function describe
```

The image defines no `HEALTHCHECK`; a stdio MCP session or a one-shot command
has nothing to probe.

## Verify the image

Each release image is signed with cosign keyless signing from the release
workflow, and its SBOM is attached as a signed attestation. Check both before
first use:

```bash
docker buildx imagetools inspect ghcr.io/paintbrushv/plat-operations:0.1.1   # shows the digest
cosign verify \
  --certificate-identity "https://github.com/paintbrushv/plat-operations/.github/workflows/image.yml@refs/tags/v0.1.1" \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  ghcr.io/paintbrushv/plat-operations:0.1.1
```

Then refer to the image by digest
(`ghcr.io/paintbrushv/plat-operations@sha256:...`) in your commands and
configuration. [DATA_FLOW.md](DATA_FLOW.md#8-how-to-verify-the-image) covers
the SBOM and what the signature does and does not prove.

## Build it yourself

From a checkout of the repository:

```bash
docker build --build-arg VERSION=0.1.1 --build-arg REVISION="$(git rev-parse HEAD)" -t boxscore-exact:local .
python3 scripts/check_image.py boxscore-exact:local --expect-version 0.1.1
```

`scripts/check_image.py` is the same check the CI image job runs.

## Remove everything

```bash
docker volume rm boxscore-data        # deletes the database
docker image rm "$IMAGE"
```

See [DATA_FLOW.md](DATA_FLOW.md#7-how-to-delete) for copies that live outside
the volume.
