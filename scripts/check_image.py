"""Prove a built boxscore-exact container image works and is locked down.

Usage: python3 scripts/check_image.py IMAGE [--expect-version X.Y.Z] [--expect-revision SHA]

Runs the checks the `image` CI workflow relies on, against a local Docker
daemon, using synthetic data only:

- image config: non-root user, entrypoint, stateless default command, /data
  volume and working directory, no exposed ports, OCI labels;
- image filesystem: no shell, root-owned binary, empty /data owned by the
  runtime user, no database or data file baked in;
- behaviour, with --network none --read-only --cap-drop ALL: the default
  command prints a `describe` envelope with status `calculated`; init,
  import-csv and review on a named volume produce `review_required` with the
  expected NOI bridge; the database file is mode 0600 and owned by the runtime
  user; MCP over stdio lists the catalog tools and reads the stored revision;
- backup and restore with `docker cp` reproduce the same review;
- an attempt to start a shell fails;
- `serve`, the only check that needs a network (--network bridge, published
  to host loopback only, otherwise the same flags), with a synthetic API key
  file mounted read-only: /health and the OAuth protected-resource metadata
  answer without a token, POST /mcp without a token gets 401 with the
  WWW-Authenticate challenge and with the token completes initialize and a
  tool call; afterwards /data holds nothing beyond what the default command
  writes there, the logs do not contain the secret, and SIGTERM stops the
  server with exit code 0.

The expected tool set is the list of functions the image's own `describe`
returns; MCP must list exactly those.

Exit 0 when every check passes; otherwise print the failed check and exit 1.
"""

import argparse
import http.client
import io
import json
import os
import secrets
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.error
import urllib.request
import uuid

RUNTIME_UID = 65532
ENTRYPOINT = ["/usr/local/bin/boxscore-exact"]
DEFAULT_CMD = ["call", "--function", "describe"]
HARDENED = [
    "--network",
    "none",
    "--read-only",
    "--cap-drop",
    "ALL",
    "--security-opt",
    "no-new-privileges",
]
# `serve` is the one check that needs a network interface; the rest of
# HARDENED still applies.
SERVE_FLAGS = ["--network", "bridge", *HARDENED[2:]]
SERVE_PORT = 3819
KEY_PATH = "/run/secrets/boxscore-keys"
METADATA_PATH = "/.well-known/oauth-protected-resource"
SHELLS = {"sh", "bash", "dash", "ash", "zsh", "busybox"}
DATA_SUFFIXES = (".sqlite", ".sqlite-journal", ".sqlite-wal", ".db", ".jsonl", ".csv")
# Functions this script calls by name. The full expected tool set is whatever
# the image's own `describe` lists (see check_stateless).
REQUIRED_TOOLS = {"describe", "noi_variance", "import_period", "review_period", "list_revisions"}
# Synthetic two-account period; the expected bridge is computed by hand.
ACTUALS = (
    "account_code,account_name,category,amount\n"
    "4000,Rent,rental income,1000.00\n"
    "5000,Repairs,repairs and maintenance,150.25\n"
)
BUDGETS = (
    "account_code,account_name,category,amount\n"
    "4000,Rent,rental income,950.00\n"
    "5000,Repairs,repairs and maintenance,120.00\n"
)
EXPECTED_BRIDGE = {
    "actual_revenue": "1000.00",
    "budget_revenue": "950.00",
    "actual_expenses": "150.25",
    "budget_expenses": "120.00",
    "actual_noi": "849.75",
    "budget_noi": "830.00",
    "noi_variance": "19.75",
}


class CheckFailed(Exception):
    pass


def check(condition, message):
    if not condition:
        raise CheckFailed(message)
    print(f"ok   {message}", flush=True)


def docker(*args, stdin=None, binary=False, allow_fail=False):
    result = subprocess.run(
        ["docker", *args],
        input=stdin,
        capture_output=True,
        text=not binary,
    )
    if result.returncode != 0 and not allow_fail:
        stderr = result.stderr if not binary else result.stderr.decode(errors="replace")
        raise CheckFailed(f"docker {' '.join(args)} exited {result.returncode}: {stderr.strip()}")
    return result


def run_json(image, *args, mounts=(), stdin=None):
    flags = ["run", "--rm", *HARDENED]
    if stdin is not None:
        flags.append("-i")
    for mount in mounts:
        flags += ["-v", mount]
    result = docker(*flags, image, *args, stdin=stdin)
    return json.loads(result.stdout)


def check_config(image, expect_version, expect_revision):
    [info] = json.loads(docker("image", "inspect", image).stdout)
    config = info["Config"]
    check(config.get("User") == f"{RUNTIME_UID}:{RUNTIME_UID}", "image runs as uid/gid 65532 (non-root)")
    check(config.get("Entrypoint") == ENTRYPOINT, "entrypoint is the boxscore-exact binary")
    check(config.get("Cmd") == DEFAULT_CMD, "default command is the stateless describe call")
    check(not config.get("ExposedPorts"), "no ports are exposed")
    check("/data" in (config.get("Volumes") or {}), "/data is declared as a volume")
    check(config.get("WorkingDir") == "/data", "working directory is /data")
    labels = config.get("Labels") or {}
    prefix = "org.opencontainers.image."
    for key in ("title", "description", "version", "revision", "source", "licenses"):
        check(labels.get(prefix + key), f"OCI label {key} is set")
    check(labels[prefix + "licenses"] == "Apache-2.0", "license label is Apache-2.0")
    if expect_version:
        check(labels[prefix + "version"] == expect_version, f"version label is {expect_version}")
    if expect_revision:
        check(labels[prefix + "revision"] == expect_revision, "revision label is the built commit")


def check_filesystem(image):
    container = docker("create", image).stdout.strip()
    try:
        archive = docker("export", container, binary=True).stdout
    finally:
        docker("rm", "-v", container)
    members = {m.name.lstrip("./").rstrip("/"): m for m in tarfile.open(fileobj=io.BytesIO(archive))}
    shells = sorted(n for n in members if os.path.basename(n) in SHELLS)
    check(not shells, f"no shell in the image filesystem {shells or ''}".rstrip())
    binary = members.get("usr/local/bin/boxscore-exact")
    check(binary is not None and binary.isfile(), "binary is at /usr/local/bin/boxscore-exact")
    check(binary.uid == 0 and binary.mode & 0o022 == 0, "binary is root-owned and not writable by others")
    data = members.get("data")
    check(data is not None and data.isdir(), "/data exists in the image")
    check(data.uid == RUNTIME_UID and data.gid == RUNTIME_UID, "/data is owned by the runtime user")
    check(data.mode & 0o777 == 0o700, f"/data is mode 0700 (found {data.mode & 0o777:o})")
    check(not [n for n in members if n.startswith("data/")], "/data ships empty")
    baked = sorted(n for n in members if n.endswith(DATA_SUFFIXES))
    check(not baked, f"no database or data file is baked into the image {baked or ''}".rstrip())
    owned = sorted(
        n for n, m in members.items()
        if m.uid == RUNTIME_UID and n != "data" and not n.startswith("home/nonroot")
    )
    check(not owned, f"nothing else is owned by the runtime user {owned or ''}".rstrip())


def check_no_shell(image):
    for shell in ("/bin/sh", "/bin/bash", "/busybox/sh"):
        result = docker("run", "--rm", "--entrypoint", shell, image, "-c", "true", allow_fail=True)
        check(result.returncode != 0, f"starting {shell} fails")


def check_stateless(image, expect_version):
    """Check the default command; return the function names describe lists."""
    envelope = run_json(image)
    check(envelope.get("status") == "calculated", "default command prints a describe envelope with status calculated")
    check(envelope.get("function") == "describe", "envelope names the describe function")
    check(envelope.get("contract_version") == "plat.ops/1", "envelope carries contract plat.ops/1")
    listed = [f.get("name") for f in envelope["result"]["calculations"]]
    names = set(listed)
    check(len(names) == len(listed) and None not in names, f"describe lists {len(names)} distinct catalog functions")
    check(REQUIRED_TOOLS <= names, "describe lists every function this check calls")
    version = docker("run", "--rm", *HARDENED, image, "--version").stdout.strip()
    check(version.startswith("boxscore-exact "), "binary reports its version")
    if expect_version:
        check(version == f"boxscore-exact {expect_version}", f"binary version is {expect_version}")
        check(envelope["producer"]["version"] == expect_version, "producer version matches")
    return names


def volume_tar(container, path):
    return tarfile.open(fileobj=io.BytesIO(docker("cp", f"{container}:{path}", "-", binary=True).stdout))


def mcp(image, volume, revision_id, tools):
    lines = [
        {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "check_image", "version": "0"}}},
        {"jsonrpc": "2.0", "method": "notifications/initialized"},
        {"jsonrpc": "2.0", "id": 2, "method": "tools/list"},
        {"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "list_revisions", "arguments": {}}},
    ]
    stdin = "".join(json.dumps(line) + "\n" for line in lines)
    result = docker(
        "run", "--rm", "-i", *HARDENED, "-v", f"{volume}:/data", image,
        "mcp", "--database", "/data/exact.sqlite", stdin=stdin,
    )
    replies = {r["id"]: r for r in map(json.loads, result.stdout.splitlines())}
    check(sorted(replies) == [1, 2, 3], "MCP stdio answers every request and no notification")
    listed = {t["name"] for t in replies[2]["result"]["tools"]}
    check(listed == tools, f"MCP lists exactly the {len(tools)} catalog functions describe names")
    stored = json.dumps(replies[3]["result"].get("structuredContent"))
    check(not replies[3]["result"].get("isError") and revision_id in stored, "MCP list_revisions reads the stored revision")


def check_stateful(image, tools):
    tag = uuid.uuid4().hex[:12]
    volume, restored = f"boxscore-check-{tag}", f"boxscore-check-restore-{tag}"
    containers = []
    with tempfile.TemporaryDirectory() as inputs:
        os.chmod(inputs, 0o755)
        for name, body in (("actuals.csv", ACTUALS), ("budgets.csv", BUDGETS)):
            path = os.path.join(inputs, name)
            with open(path, "w") as handle:
                handle.write(body)
            os.chmod(path, 0o644)
        try:
            docker("volume", "create", volume)
            data = f"{volume}:/data"
            created = run_json(image, "init", "--database", "/data/exact.sqlite", mounts=[data])
            check(created.get("status") == "created", "init creates a database on the volume")
            imported = run_json(
                image, "import-csv", "--database", "/data/exact.sqlite",
                "--actuals", "/in/actuals.csv", "--budgets", "/in/budgets.csv",
                "--property", "synthetic_ops", "--period", "2026-05", "--units", "10",
                mounts=[data, f"{inputs}:/in:ro"],
            )
            check(imported.get("status") == "imported", "import-csv imports the synthetic period")
            revision = imported["revision_id"]
            review = docker(
                "run", "--rm", *HARDENED, "-v", f"{data}:ro", image,
                "review", "--database", "/data/exact.sqlite", "--revision", revision,
            ).stdout
            body = json.loads(review)
            check(body.get("status") in ("calculated", "review_required"), f"review status is {body.get('status')}")
            check(body.get("status") == "review_required", "missing snapshot keeps the period in review")
            bridge = body["variance"]["noi_bridge"]
            check(all(bridge.get(k) == v for k, v in EXPECTED_BRIDGE.items()), "review returns the expected NOI bridge")
            check(body.get("human_approval") == "not_granted", "review never grants human approval")

            mcp(image, volume, revision, tools)

            holder = docker("container", "create", "-v", data, image).stdout.strip()
            containers.append(holder)
            listing = {m.name.split("/")[-1]: m for m in volume_tar(holder, "/data")}
            db = listing.get("exact.sqlite")
            check(db is not None and db.mode & 0o777 == 0o600, "database file is mode 0600")
            check(db.uid == RUNTIME_UID and db.gid == RUNTIME_UID, "database file is owned by the runtime user")
            stray = sorted(n for n in listing if n not in ("data", "exact.sqlite"))
            check(not stray, f"the volume holds only the database {stray or ''}".rstrip())

            backup = volume_tar(holder, "/data/exact.sqlite").extractfile("exact.sqlite").read()
            check(backup.startswith(b"SQLite format 3\x00"), "docker cp backup is a SQLite database")

            docker("volume", "create", restored)
            target = docker("container", "create", "-v", f"{restored}:/data", image).stdout.strip()
            containers.append(target)
            stream = io.BytesIO()
            with tarfile.open(fileobj=stream, mode="w") as archive:
                member = tarfile.TarInfo("exact.sqlite")
                member.size, member.mode = len(backup), 0o600
                member.uid = member.gid = RUNTIME_UID
                archive.addfile(member, io.BytesIO(backup))
            docker("cp", "-a", "-", f"{target}:/data", stdin=stream.getvalue(), binary=True)
            again = docker(
                "run", "--rm", *HARDENED, "-v", f"{restored}:/data:ro", image,
                "review", "--database", "/data/exact.sqlite", "--revision", revision,
            ).stdout
            check(again == review, "restored backup reproduces the review byte for byte")
        finally:
            for container in containers:
                docker("rm", "-v", container, allow_fail=True)
            for name in (volume, restored):
                docker("volume", "rm", "-f", name, allow_fail=True)


# Talk to the published port directly, never through a proxy from the environment.
OPENER = urllib.request.build_opener(urllib.request.ProxyHandler({}))


def fetch(method, url, body=None, headers=None):
    """Return (status, headers, parsed JSON body or None) without raising on 4xx."""
    data = None if body is None else json.dumps(body).encode()
    request = urllib.request.Request(url, data=data, method=method, headers=headers or {})
    try:
        with OPENER.open(request, timeout=10) as response:
            status, reply_headers, raw = response.status, response.headers, response.read()
    except urllib.error.HTTPError as error:
        status, reply_headers, raw = error.code, error.headers, error.read()
    try:
        parsed = json.loads(raw) if raw else None
    except ValueError:
        parsed = None
    return status, reply_headers, parsed


def listing(path):
    return sorted(os.path.relpath(os.path.join(root, name), path)
                  for root, dirs, files in os.walk(path) for name in dirs + files)


def check_serve(image, tools):
    # A bind-mounted file keeps its host owner, and a 0600 key file is readable
    # by that owner only. Without root on the host the file cannot be handed to
    # uid 65532 (docs/INSTALL.md does that with chown), so this check runs the
    # container as the invoking user, with /data on a directory that user owns,
    # so the server could write there if it wanted to.
    user = f"{os.getuid()}:{os.getgid()}"
    name = f"boxscore-check-serve-{uuid.uuid4().hex[:12]}"
    secret = secrets.token_hex(24)  # 48 characters; the server needs at least 32
    with tempfile.TemporaryDirectory() as work:
        key_file = os.path.join(work, "boxscore-keys")
        descriptor = os.open(key_file, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(descriptor, "w") as handle:
            handle.write(f"# synthetic key for scripts/check_image.py\nimage-check:{secret}\n")
        os.chmod(key_file, 0o600)
        data = os.path.join(work, "data")
        os.mkdir(data, 0o700)
        mount = ["--user", user, "-v", f"{data}:/data"]

        # Baseline: whatever the default command writes to /data.
        docker("run", "--rm", *HARDENED, *mount, image)
        baseline = listing(data)

        try:
            docker(
                "run", "-d", "--name", name, *SERVE_FLAGS, *mount,
                "-v", f"{key_file}:{KEY_PATH}:ro",
                "-p", f"127.0.0.1::{SERVE_PORT}",
                image, "serve", "--bind", f"0.0.0.0:{SERVE_PORT}", "--api-key-file", KEY_PATH,
            )
            published = docker("port", name, f"{SERVE_PORT}/tcp").stdout.split()[0]
            host, _, port = published.rpartition(":")
            check(host == "127.0.0.1", f"serve is published on host loopback only ({published})")
            base = f"http://127.0.0.1:{port}"

            deadline = time.monotonic() + 30
            status = None
            while time.monotonic() < deadline:
                try:
                    status, _, health = fetch("GET", f"{base}/health")
                    if status == 200:
                        break
                except (OSError, http.client.HTTPException):
                    pass
                running = docker("inspect", "-f", "{{.State.Running}}", name).stdout.strip()
                if running != "true":
                    logs = docker("logs", name, allow_fail=True).stderr.strip()
                    raise CheckFailed(f"serve exited before answering: {logs}")
                time.sleep(0.5)
            check(status == 200 and health.get("status") == "ok", "GET /health is 200 without a token")

            status, _, metadata = fetch("GET", f"{base}{METADATA_PATH}")
            check(status == 200 and "boxscore:calc" in (metadata or {}).get("scopes_supported", []),
                  "GET /.well-known/oauth-protected-resource is 200 without a token")

            mcp_headers = {
                "Content-Type": "application/json",
                "Accept": "application/json, text/event-stream",
            }
            initialize = {
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                           "clientInfo": {"name": "check_image", "version": "0"}},
            }
            status, headers, _ = fetch("POST", f"{base}/mcp", initialize, mcp_headers)
            challenge = headers.get("WWW-Authenticate", "")
            check(status == 401, "POST /mcp without a token is 401")
            check(challenge.startswith("Bearer resource_metadata=") and METADATA_PATH in challenge,
                  "the 401 carries the WWW-Authenticate challenge")

            authorized = {**mcp_headers, "Authorization": f"Bearer {secret}"}
            status, _, reply = fetch("POST", f"{base}/mcp", initialize, authorized)
            result = (reply or {}).get("result") or {}
            check(status == 200 and (reply or {}).get("id") == 1 and result.get("serverInfo", {}).get("name") == "boxscore-exact",
                  "POST /mcp with the token completes initialize")
            versioned = {**authorized, "MCP-Protocol-Version": "2025-06-18"}
            status, _, _ = fetch("POST", f"{base}/mcp", {"jsonrpc": "2.0", "method": "notifications/initialized"}, versioned)
            check(status == 202, "the initialized notification is accepted")
            call = {"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "describe", "arguments": {}}}
            status, _, reply = fetch("POST", f"{base}/mcp", call, versioned)
            envelope = ((reply or {}).get("result") or {}).get("structuredContent") or {}
            names = {c.get("name") for c in (envelope.get("result") or {}).get("calculations", [])}
            check(status == 200 and envelope.get("status") == "calculated" and names == tools,
                  "an authenticated tools/call over HTTP returns the same catalog")
            check(envelope.get("trace_store") == "none", "with no database and no trace directory nothing is traced")

            docker("stop", "--time", "10", name)
            code = docker("inspect", "-f", "{{.State.ExitCode}}", name).stdout.strip()
            check(code == "0", "SIGTERM stops serve with exit code 0")
            logs = docker("logs", name)
            check(secret not in logs.stdout + logs.stderr, "the server logs do not contain the API key secret")
        finally:
            docker("rm", "-f", "-v", name, allow_fail=True)
        after = listing(data)
        check(after == baseline, f"serve wrote nothing to /data beyond the default command {after or ''}".rstrip())


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("image")
    parser.add_argument("--expect-version")
    parser.add_argument("--expect-revision")
    args = parser.parse_args()
    try:
        check_config(args.image, args.expect_version, args.expect_revision)
        check_filesystem(args.image)
        check_no_shell(args.image)
        tools = check_stateless(args.image, args.expect_version)
        check_stateful(args.image, tools)
        check_serve(args.image, tools)
    except CheckFailed as failure:
        print(f"FAIL {failure}", file=sys.stderr)
        return 1
    print("all image checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
