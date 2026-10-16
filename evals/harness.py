"""Plumbing for the agent grounding evals: prepare a database, speak MCP.

Standard library only. Nothing here imports the Anthropic SDK or reads a key.

The MCP client mirrors boxscore/tests/exact_mcp.rs: one JSON-RPC 2.0 message
per line on the server's stdin and stdout, `initialize` then the
`notifications/initialized` notification, then requests answered in order.
"""
from __future__ import annotations

import copy
import json
import os
import queue
import re
import subprocess
import threading
from pathlib import Path
from typing import Any

EVALS_DIR = Path(__file__).resolve().parent
REPO_ROOT = EVALS_DIR.parent
DEFAULT_BINARY = REPO_ROOT / "boxscore" / "target" / "release" / "boxscore-exact"
PROTOCOL_VERSION = "2025-06-18"
GROUNDING_PROMPT = "boxscore_grounding"
ANALYST_SENTENCE = "The user is an asset-management analyst."

# Environment variables that would change what the server writes or prints.
_SCRUBBED_ENV = ("BOXSCORE_MCP_DEBUG", "BOXSCORE_TRACE_DIR", "BOXSCORE_TRACE_INPUTS")


class HarnessError(RuntimeError):
    """Setup or protocol failure; the scenario is recorded as errored."""


def find_binary(explicit: str | None = None) -> Path:
    candidate = explicit or os.environ.get("BOXSCORE_EXACT_BIN") or str(DEFAULT_BINARY)
    path = Path(candidate)
    if not (path.is_file() and os.access(path, os.X_OK)):
        raise HarnessError(
            f"boxscore-exact binary not found or not executable: {path} "
            "(build it: cd boxscore && cargo build --release --bin boxscore-exact; "
            "or pass --binary / set BOXSCORE_EXACT_BIN)"
        )
    return path


def _clean_env() -> dict[str, str]:
    env = dict(os.environ)
    for name in _SCRUBBED_ENV:
        env.pop(name, None)
    # The model key never reaches the engine.
    env.pop("ANTHROPIC_API_KEY", None)
    return env


# ---------------------------------------------------------------- scenarios

def load_scenarios(directory: Path | None = None) -> list[dict[str, Any]]:
    directory = directory or (EVALS_DIR / "scenarios")
    scenarios = []
    for path in sorted(directory.glob("*.json")):
        scenario = json.loads(path.read_text(encoding="utf-8"))
        scenario["_file"] = path.name
        scenarios.append(scenario)
    return scenarios


_PLACEHOLDER = re.compile(r"\{(db|repo|evals|tmp|rev\.[A-Za-z0-9_]+)\}")


def substitute(value: Any, context: dict[str, str]) -> Any:
    """Replace {db}, {repo}, {evals}, {tmp} and {rev.<alias>} in strings,
    recursively through lists and dicts. An unknown placeholder is an error."""
    if isinstance(value, str):
        def repl(match: re.Match[str]) -> str:
            key = match.group(1)
            if key not in context:
                raise HarnessError(f"unknown placeholder {{{key}}}")
            return context[key]

        return _PLACEHOLDER.sub(repl, value)
    if isinstance(value, list):
        return [substitute(item, context) for item in value]
    if isinstance(value, dict):
        return {key: substitute(item, context) for key, item in value.items()}
    return value


def prepare_database(
    scenario: dict[str, Any], binary: Path, workdir: Path
) -> dict[str, Any]:
    """Run the scenario's setup steps on a fresh database in `workdir`.

    Each step is {"args": [...], "save_revision_as": "alias"?}. The args are
    passed to the binary as an argument vector (no shell). A step that saves
    an alias must print JSON with a `revision_id`. Returns the database path,
    the aliases and a record of every step.
    """
    database = workdir / "exact.sqlite"
    context = {
        "db": str(database),
        "repo": str(REPO_ROOT),
        "evals": str(EVALS_DIR),
        "tmp": str(workdir),
    }
    steps_out = []
    for index, step in enumerate(scenario.get("setup", [])):
        args = substitute(step["args"], context)
        proc = subprocess.run(
            [str(binary), *args],
            capture_output=True,
            text=True,
            cwd=workdir,
            env=_clean_env(),
            timeout=180,
        )
        record: dict[str, Any] = {
            "args": step["args"],
            "exit_code": proc.returncode,
            "stdout": proc.stdout.strip(),
        }
        if proc.stderr.strip():
            record["stderr"] = proc.stderr.strip()
        steps_out.append(record)
        if proc.returncode != 0:
            raise HarnessError(
                f"setup step {index} {step['args'][0]} exited {proc.returncode}: "
                f"{proc.stdout.strip() or proc.stderr.strip()}"
            )
        alias = step.get("save_revision_as")
        if alias:
            try:
                context[f"rev.{alias}"] = json.loads(proc.stdout)["revision_id"]
            except (ValueError, KeyError, TypeError) as exc:
                raise HarnessError(
                    f"setup step {index} did not print a revision_id: {proc.stdout!r}"
                ) from exc
    aliases = {k[len("rev."):]: v for k, v in context.items() if k.startswith("rev.")}
    return {"database": str(database), "aliases": aliases, "steps": steps_out}


# --------------------------------------------------------------- MCP client

class McpSession:
    """`boxscore-exact mcp --database DB` as a subprocess, spoken to over stdio."""

    def __init__(self, binary: Path, database: Path | str, timeout: float = 120.0):
        self.timeout = timeout
        self.returncode: int | None = None
        self._next_id = 0
        self._lines: queue.Queue[str | None] = queue.Queue()
        self._proc = subprocess.Popen(
            [str(binary), "mcp", "--database", str(database)],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            encoding="utf-8",
            env=_clean_env(),
        )
        threading.Thread(target=self._pump, daemon=True).start()
        self._stderr: list[str] = []
        threading.Thread(target=self._drain_stderr, daemon=True).start()

    def _pump(self) -> None:
        assert self._proc.stdout is not None
        for line in self._proc.stdout:
            self._lines.put(line)
        self._lines.put(None)

    def _drain_stderr(self) -> None:
        assert self._proc.stderr is not None
        for line in self._proc.stderr:
            self._stderr.append(line)

    def _send(self, message: dict[str, Any]) -> None:
        assert self._proc.stdin is not None
        self._proc.stdin.write(json.dumps(message, separators=(",", ":")) + "\n")
        self._proc.stdin.flush()

    def notify(self, method: str) -> None:
        self._send({"jsonrpc": "2.0", "method": method})

    def request(self, method: str, params: dict[str, Any] | None = None) -> dict[str, Any]:
        self._next_id += 1
        request_id = self._next_id
        self._send(
            {"jsonrpc": "2.0", "id": request_id, "method": method, "params": params or {}}
        )
        try:
            line = self._lines.get(timeout=self.timeout)
        except queue.Empty as exc:
            raise HarnessError(f"MCP server did not answer {method} in {self.timeout}s") from exc
        if line is None:
            raise HarnessError(f"MCP server closed stdout during {method}")
        try:
            response = json.loads(line)
        except ValueError as exc:
            raise HarnessError(f"MCP server wrote a non-JSON line: {line!r}") from exc
        if response.get("id") != request_id:
            raise HarnessError(f"MCP response id mismatch for {method}: {response!r}")
        return response

    # -- the methods the evals use

    def handshake(self) -> dict[str, Any]:
        response = self.request(
            "initialize",
            {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "boxscore-evals", "version": "0"},
            },
        )
        if "result" not in response:
            raise HarnessError(f"initialize failed: {response!r}")
        self.notify("notifications/initialized")
        return response["result"]

    def list_tools(self) -> list[dict[str, Any]]:
        response = self.request("tools/list")
        if "result" not in response:
            raise HarnessError(f"tools/list failed: {response!r}")
        return response["result"]["tools"]

    def get_prompt_text(self, name: str = GROUNDING_PROMPT) -> str:
        response = self.request("prompts/get", {"name": name})
        if "result" not in response:
            raise HarnessError(f"prompts/get {name} failed: {response!r}")
        message = response["result"]["messages"][0]
        return message["content"]["text"]

    def call_tool(self, name: str, arguments: dict[str, Any]) -> dict[str, Any]:
        """Returns {"request", "rpc_error"?, "result"?, "envelope"?, "content_text"?}.

        A JSON-RPC error (an unknown tool, bad params) has no envelope. An
        engine refusal is a normal result with isError true and an envelope
        whose status is "refused".
        """
        response = self.request("tools/call", {"name": name, "arguments": arguments})
        record: dict[str, Any] = {"request": {"name": name, "arguments": arguments}}
        if "error" in response:
            record["rpc_error"] = response["error"]
            record["envelope"] = None
            return record
        result = response["result"]
        record["result"] = result
        record["envelope"] = result.get("structuredContent")
        content = result.get("content") or []
        if content and content[0].get("type") == "text":
            record["content_text"] = content[0]["text"]
        return record

    def close(self) -> str:
        """Close stdin (the server exits when it closes) and return stderr."""
        try:
            if self._proc.stdin and not self._proc.stdin.closed:
                self._proc.stdin.close()
            self._proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self._proc.kill()
            self._proc.wait()
        self.returncode = self._proc.returncode
        return "".join(self._stderr)

    def __enter__(self) -> "McpSession":
        return self

    def __exit__(self, *exc: object) -> None:
        self.close()


# ------------------------------------------------- MCP tool -> Anthropic tool

_COMBINATORS = ("oneOf", "anyOf", "allOf")


def to_anthropic_tool(tool: dict[str, Any]) -> dict[str, Any]:
    """Convert one MCP tool to an Anthropic tool definition.

    name, description and inputSchema map one to one (`inputSchema` becomes
    `input_schema`). Two changes are forced by the Messages API: the
    `$schema` keyword is dropped, and a top-level `oneOf`/`anyOf`/`allOf` is
    removed because tool input schemas must be a plain object schema. The
    server's own schema still validates every call. To keep what the model is
    told equal to what the server enforces, the alternatives that were removed
    are restated in the description ("Provide one of: revision_id | snapshot
    and unit_count").
    """
    schema = copy.deepcopy(tool.get("inputSchema") or {"type": "object", "properties": {}})
    schema.pop("$schema", None)
    description = tool.get("description", "")
    removed = [key for key in _COMBINATORS if key in schema]
    if removed:
        alternatives = []
        for key in removed:
            for branch in schema.pop(key):
                required = branch.get("required") if isinstance(branch, dict) else None
                if required:
                    alternatives.append(" and ".join(required))
        if alternatives:
            description += (
                "\n\nInput rule enforced by the server: provide exactly one of these "
                "argument sets: " + " | ".join(alternatives) + "."
            )
    return {"name": tool["name"], "description": description, "input_schema": schema}


def system_prompt(grounding_text: str) -> str:
    return f"{grounding_text}\n\n{ANALYST_SENTENCE}"
