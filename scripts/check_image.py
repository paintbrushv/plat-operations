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
- an attempt to start a shell fails.

Exit 0 when every check passes; otherwise print the failed check and exit 1.
"""

import argparse
import io
import json
import os
import subprocess
import sys
import tarfile
import tempfile
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
SHELLS = {"sh", "bash", "dash", "ash", "zsh", "busybox"}
DATA_SUFFIXES = (".sqlite", ".sqlite-journal", ".sqlite-wal", ".db", ".jsonl", ".csv")
TOOLS = {
    "describe",
    "noi_variance",
    "import_period",
    "review_period",
    "issue_report",
    "list_revisions",
}
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
    check(data.mode & 0o777 == 0o700, "/data is mode 0700")
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
    envelope = run_json(image)
    check(envelope.get("status") == "calculated", "default command prints a describe envelope with status calculated")
    check(envelope.get("function") == "describe", "envelope names the describe function")
    check(envelope.get("contract_version") == "plat.ops/1", "envelope carries contract plat.ops/1")
    names = {f.get("name") for f in envelope["result"]["calculations"]}
    check(names == TOOLS, "describe lists the six catalog functions")
    version = docker("run", "--rm", *HARDENED, image, "--version").stdout.strip()
    check(version.startswith("boxscore-exact "), "binary reports its version")
    if expect_version:
        check(version == f"boxscore-exact {expect_version}", f"binary version is {expect_version}")
        check(envelope["producer"]["version"] == expect_version, "producer version matches")


def volume_tar(container, path):
    return tarfile.open(fileobj=io.BytesIO(docker("cp", f"{container}:{path}", "-", binary=True).stdout))


def mcp(image, volume, revision_id):
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
    tools = {t["name"] for t in replies[2]["result"]["tools"]}
    check(tools == TOOLS, "MCP lists exactly the six catalog tools")
    listed = json.dumps(replies[3]["result"].get("structuredContent"))
    check(not replies[3]["result"].get("isError") and revision_id in listed, "MCP list_revisions reads the stored revision")


def check_stateful(image):
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

            mcp(image, volume, revision)

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
        check_stateless(args.image, args.expect_version)
        check_stateful(args.image)
    except CheckFailed as failure:
        print(f"FAIL {failure}", file=sys.stderr)
        return 1
    print("all image checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
