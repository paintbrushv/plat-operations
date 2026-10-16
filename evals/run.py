#!/usr/bin/env python3
"""Run the agent grounding evals.

For each scenario: build a fresh database in a temp directory from the
scenario's setup steps, start `boxscore-exact mcp --database DB` as a
subprocess speaking JSON-RPC over stdio, hand its tools to a model through the
Anthropic Messages API, run the tool-use loop (at most 8 tool calls) and write
a transcript to evals/out/<timestamp>/<id>.json. Scoring is separate:
`python3 evals/report.py evals/out/<timestamp>`.

`--dry-run` does everything except call the model, and needs no key.
A live run needs ANTHROPIC_API_KEY in the environment (it is never read from a
file, never written anywhere) and the `anthropic` package; without the key
this exits 2 before doing anything.

Exit status: 0 all scenarios completed, 1 a scenario errored or the token
budget stopped the run, 2 usage error or no API key.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import sys
import tempfile
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

import harness

DEFAULT_MODEL = "claude-sonnet-5-5"
MAX_TOOL_CALLS = 8
SCHEMA = "boxscore.evals.transcript/1"
GROUNDING_RULE_HEAD = "Numbers come from the engine."


class BudgetExceeded(Exception):
    pass


def utc_now() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="seconds")


def block_to_dict(block: Any) -> dict[str, Any]:
    """An SDK content block as a plain dict, safe to send back and to record."""
    if isinstance(block, dict):
        return block
    if hasattr(block, "model_dump"):
        return block.model_dump(exclude_none=True)
    return {"type": getattr(block, "type", "unknown")}


class Usage:
    def __init__(self) -> None:
        self.input_tokens = 0
        self.output_tokens = 0
        self.cache_creation_input_tokens = 0
        self.cache_read_input_tokens = 0

    def add(self, usage: Any) -> None:
        for name in vars(self):
            setattr(self, name, getattr(self, name) + (getattr(usage, name, 0) or 0))

    @property
    def total(self) -> int:
        return self.input_tokens + self.output_tokens

    def as_dict(self) -> dict[str, int]:
        return {**vars(self), "total_tokens": self.total}


def run_scenario(
    scenario: dict[str, Any],
    *,
    binary: Path,
    client: Any,
    model: str,
    dry_run: bool,
    run_usage: Usage,
    max_total_tokens: int | None,
    max_tool_calls: int,
) -> dict[str, Any]:
    transcript: dict[str, Any] = {
        "schema": SCHEMA,
        "scenario_id": scenario["id"],
        "title": scenario["title"],
        "scenario": {k: v for k, v in scenario.items() if not k.startswith("_")},
        "model": None if dry_run else model,
        "dry_run": dry_run,
        "started_at": utc_now(),
        "setup": None,
        "mcp": None,
        "system_prompt": None,
        "tool_calls": [],
        "assistant_turns": [],
        "model_turns": 0,
        "usage": Usage().as_dict(),
        "stop_reason": None,
        "answer": None,
        "error": None,
    }
    started = time.monotonic()
    usage = Usage()
    try:
        with tempfile.TemporaryDirectory(prefix="boxscore-eval-") as workdir:
            setup = harness.prepare_database(scenario, binary, Path(workdir))
            transcript["setup"] = {"aliases": setup["aliases"], "steps": setup["steps"]}
            with harness.McpSession(binary, setup["database"]) as mcp:
                init = mcp.handshake()
                mcp_tools = mcp.list_tools()
                grounding = mcp.get_prompt_text(harness.GROUNDING_PROMPT)
                if not grounding.startswith(GROUNDING_RULE_HEAD):
                    raise harness.HarnessError("grounding prompt does not start with the grounding rule")
                system = harness.system_prompt(grounding)
                transcript["system_prompt"] = system
                transcript["mcp"] = {
                    "protocolVersion": init.get("protocolVersion"),
                    "serverInfo": init.get("serverInfo"),
                    "tools": mcp_tools,
                    "prompt": harness.GROUNDING_PROMPT,
                    "prompt_sha256": hashlib.sha256(grounding.encode()).hexdigest(),
                }
                names = {t["name"] for t in mcp_tools}
                wanted = []
                for spec in (scenario.get("expect") or {}).get("tools", []):
                    fn = spec["function"]
                    wanted += [fn] if isinstance(fn, str) else list(fn)
                unknown = sorted(set(wanted) - names)
                if unknown:
                    raise harness.HarnessError(f"scenario expects tools the server does not offer: {unknown}")
                if not dry_run:
                    _tool_loop(
                        transcript, scenario, mcp, client, model, system,
                        [harness.to_anthropic_tool(t) for t in mcp_tools],
                        usage, run_usage, max_total_tokens, max_tool_calls,
                    )
                # Close stdin as a client does: the server must exit 0 and say nothing on stderr.
                transcript["mcp"]["stderr"] = mcp.close()
                transcript["mcp"]["exit_code"] = mcp.returncode
    except BudgetExceeded as exc:
        transcript["error"] = f"budget_exceeded: {exc}"
    except Exception as exc:  # recorded, never raised: one scenario must not stop the rest
        transcript["error"] = f"{type(exc).__name__}: {exc}"
    transcript["usage"] = usage.as_dict()
    transcript["finished_at"] = utc_now()
    transcript["duration_seconds"] = round(time.monotonic() - started, 2)
    return transcript


def _tool_loop(
    transcript: dict[str, Any],
    scenario: dict[str, Any],
    mcp: harness.McpSession,
    client: Any,
    model: str,
    system: str,
    tools: list[dict[str, Any]],
    usage: Usage,
    run_usage: Usage,
    max_total_tokens: int | None,
    max_tool_calls: int,
) -> None:
    max_tokens = int((scenario.get("expect") or {}).get("max_tokens", 1024))
    messages: list[dict[str, Any]] = [{"role": "user", "content": scenario["question"]}]
    calls_made = 0
    capped_turns = 0
    while True:
        if max_total_tokens is not None and run_usage.total >= max_total_tokens:
            raise BudgetExceeded(f"{run_usage.total} tokens used, limit {max_total_tokens}")
        response = client.messages.create(
            model=model, max_tokens=max_tokens, system=system, tools=tools, messages=messages
        )
        usage.add(response.usage)
        run_usage.add(response.usage)
        transcript["model_turns"] += 1
        content = [block_to_dict(b) for b in response.content]
        text = "".join(b.get("text", "") for b in content if b.get("type") == "text")
        transcript["assistant_turns"].append(
            {"stop_reason": response.stop_reason, "text": text,
             "tool_uses": [b["name"] for b in content if b.get("type") == "tool_use"]}
        )
        transcript["stop_reason"] = response.stop_reason
        uses = [b for b in content if b.get("type") == "tool_use"]
        if response.stop_reason != "tool_use" or not uses:
            transcript["answer"] = text
            return
        results = []
        capped_this_turn = False
        for use in uses:
            if calls_made >= max_tool_calls:
                capped_this_turn = True
                results.append({
                    "type": "tool_result", "tool_use_id": use["id"], "is_error": True,
                    "content": f"Tool call limit ({max_tool_calls}) reached; answer with what you already have.",
                })
                continue
            calls_made += 1
            record = mcp.call_tool(use["name"], use.get("input") or {})
            record["id"] = use["id"]
            transcript["tool_calls"].append(record)
            if "rpc_error" in record:
                body, is_error = json.dumps(record["rpc_error"]), True
            else:
                body = record.get("content_text") or json.dumps(record["result"])
                is_error = bool(record["result"].get("isError"))
            results.append({"type": "tool_result", "tool_use_id": use["id"], "content": body, "is_error": is_error})
        messages.append({"role": "assistant", "content": content})
        messages.append({"role": "user", "content": results})
        if capped_this_turn:
            transcript["tool_call_cap_hit"] = True
            capped_turns += 1
            if capped_turns >= 2:
                transcript["answer"] = text
                return


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--model", default=DEFAULT_MODEL, help=f"model id (default {DEFAULT_MODEL})")
    parser.add_argument("--scenario", action="append", default=[], metavar="ID",
                        help="run only this scenario id (repeatable); default all")
    parser.add_argument("--max-total-tokens", type=int, default=None, metavar="N",
                        help="stop the run when input+output tokens used exceed N")
    parser.add_argument("--out", default=str(harness.EVALS_DIR / "out"), help="output root (default evals/out)")
    parser.add_argument("--dry-run", action="store_true",
                        help="prepare databases, start the MCP server, list tools, fetch the prompt; no model call, no key")
    parser.add_argument("--binary", default=None, help="path to boxscore-exact (or BOXSCORE_EXACT_BIN)")
    parser.add_argument("--max-tool-calls", type=int, default=MAX_TOOL_CALLS, help="tool calls per scenario (default 8)")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)
    client = None
    if not args.dry_run:
        if not os.environ.get("ANTHROPIC_API_KEY"):
            print("ANTHROPIC_API_KEY is not set: not calling the API. "
                  "Set it in the environment (never in a file) or use --dry-run.", file=sys.stderr)
            return 2
        try:
            import anthropic  # lazy: the scorer and its tests never need the SDK
        except ImportError:
            print("The `anthropic` package is not installed: pip install -r evals/requirements.txt", file=sys.stderr)
            return 2
        client = anthropic.Anthropic()  # reads ANTHROPIC_API_KEY from the environment

    try:
        binary = harness.find_binary(args.binary)
    except harness.HarnessError as exc:
        print(str(exc), file=sys.stderr)
        return 2

    scenarios = harness.load_scenarios()
    if args.scenario:
        known = {s["id"] for s in scenarios}
        unknown = [s for s in args.scenario if s not in known]
        if unknown:
            print(f"unknown scenario id(s): {', '.join(unknown)}; known: {', '.join(sorted(known))}", file=sys.stderr)
            return 2
        scenarios = [s for s in scenarios if s["id"] in args.scenario]

    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ") + ("-dry-run" if args.dry_run else "")
    out_dir = Path(args.out) / stamp
    out_dir.mkdir(parents=True, exist_ok=True)
    print(f"{'dry run' if args.dry_run else 'live run'}: {len(scenarios)} scenario(s), binary {binary}, output {out_dir}")

    run_usage = Usage()
    errors = 0
    summary = []
    for index, scenario in enumerate(scenarios):
        if args.max_total_tokens is not None and run_usage.total >= args.max_total_tokens:
            print(f"token budget reached ({run_usage.total} >= {args.max_total_tokens}); "
                  f"skipping {len(scenarios) - index} scenario(s)", file=sys.stderr)
            errors += 1
            summary += [{"scenario_id": s["id"], "skipped": "budget_exceeded"} for s in scenarios[index:]]
            break
        transcript = run_scenario(
            scenario, binary=binary, client=client, model=args.model, dry_run=args.dry_run,
            run_usage=run_usage, max_total_tokens=args.max_total_tokens, max_tool_calls=args.max_tool_calls,
        )
        (out_dir / f"{scenario['id']}.json").write_text(
            json.dumps(transcript, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
        if transcript["error"]:
            errors += 1
        mcp = transcript.get("mcp") or {}
        status = "ERROR " + transcript["error"] if transcript["error"] else "ok"
        print(
            f"  {scenario['id']:<40} {status}"
            f" | setup steps {len((transcript.get('setup') or {}).get('steps', []))}"
            f", tools {len(mcp.get('tools', []))}"
            f", model turns {transcript['model_turns']}"
            f", tool calls {len(transcript['tool_calls'])}"
            f", tokens {transcript['usage']['total_tokens']}"
        )
        summary.append({"scenario_id": scenario["id"], "error": transcript["error"],
                        "tokens": transcript["usage"]["total_tokens"]})

    (out_dir / "_run.json").write_text(json.dumps({
        "schema": "boxscore.evals.run/1",
        "model": None if args.dry_run else args.model,
        "dry_run": args.dry_run,
        "finished_at": utc_now(),
        "max_total_tokens": args.max_total_tokens,
        "usage": run_usage.as_dict(),
        "scenarios": summary,
    }, indent=2) + "\n", encoding="utf-8")
    print(f"{len(scenarios)} scenario(s), {errors} errored, {run_usage.total} tokens. Output: {out_dir}")
    if not args.dry_run:
        print(f"Score it: python3 evals/report.py {out_dir}")
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
