#!/usr/bin/env python3
"""Score a run directory and print a report.

    python3 evals/report.py evals/out/<timestamp> [--json PATH] [--markdown PATH]

Prints the markdown table, writes report.json and report.md beside the
transcripts unless told otherwise. Exit 1 if any scenario failed or errored.
Standard library only; never touches the network.
"""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any

import score


def load_transcripts(directory: Path) -> list[dict[str, Any]]:
    transcripts = []
    for path in sorted(directory.glob("*.json")):
        if path.name.startswith("_") or path.name.startswith("report"):
            continue
        transcripts.append(json.loads(path.read_text(encoding="utf-8")))
    return transcripts


def build_report(transcripts: list[dict[str, Any]]) -> dict[str, Any]:
    rows = []
    for transcript in transcripts:
        result = score.score_transcript(transcript)
        usage = transcript.get("usage") or {}
        rows.append({
            "scenario_id": result["scenario_id"],
            "title": transcript.get("title"),
            "dry_run": bool(result.get("dry_run")),
            "errored": bool(transcript.get("error")),
            "error": transcript.get("error"),
            "passed": result["passed"],
            "failing": result["failing"],
            "invented_numbers": result.get("invented_numbers", []),
            "tokens": usage.get("total_tokens", 0),
            "input_tokens": usage.get("input_tokens", 0),
            "output_tokens": usage.get("output_tokens", 0),
            "tool_calls": len(transcript.get("tool_calls", [])),
            "checks": result["checks"],
        })
    scored = [r for r in rows if not r["dry_run"]]
    failing_by_check: dict[str, int] = {}
    for row in scored:
        for name in row["failing"]:
            failing_by_check[name] = failing_by_check.get(name, 0) + 1
    models = sorted({t.get("model") for t in transcripts if t.get("model")})
    return {
        "models": models,
        "totals": {
            "scenarios": len(rows),
            "dry_run": len(rows) - len(scored),
            "scored": len(scored),
            "passed": sum(1 for r in scored if r["passed"]),
            "failed": sum(1 for r in scored if r["passed"] is False),
            "errored": sum(1 for r in rows if r["errored"]),
            "invented_numbers": sum(len(r["invented_numbers"]) for r in scored),
            "tokens": sum(r["tokens"] for r in rows),
            "input_tokens": sum(r["input_tokens"] for r in rows),
            "output_tokens": sum(r["output_tokens"] for r in rows),
            "failing_by_check": dict(sorted(failing_by_check.items())),
        },
        "scenarios": rows,
    }


def render_markdown(report: dict[str, Any]) -> str:
    totals = report["totals"]
    lines = [
        "| scenario | result | failing checks | invented numbers | tokens |",
        "|---|---|---|---|---|",
    ]
    for row in report["scenarios"]:
        if row["dry_run"]:
            result = "dry run"
        elif row["errored"]:
            result = "ERROR"
        else:
            result = "pass" if row["passed"] else "FAIL"
        failing = ", ".join(row["failing"]) or "-"
        if row["errored"]:
            failing = (row["error"] or "")[:80]
        invented = ", ".join(row["invented_numbers"]) or "-"
        lines.append(f"| {row['scenario_id']} | {result} | {failing} | {invented} | {row['tokens']} |")
    lines.append("")
    lines.append(
        f"{totals['passed']} passed, {totals['failed']} failed, {totals['errored']} errored, "
        f"{totals['dry_run']} dry run, of {totals['scenarios']} scenarios; "
        f"{totals['invented_numbers']} invented number(s); {totals['tokens']} tokens "
        f"({totals['input_tokens']} in, {totals['output_tokens']} out)."
    )
    return "\n".join(lines) + "\n"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("directory", type=Path)
    parser.add_argument("--json", type=Path, default=None)
    parser.add_argument("--markdown", type=Path, default=None)
    args = parser.parse_args(argv)
    if not args.directory.is_dir():
        print(f"not a directory: {args.directory}", file=sys.stderr)
        return 2
    report = build_report(load_transcripts(args.directory))
    markdown = render_markdown(report)
    (args.json or args.directory / "report.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    (args.markdown or args.directory / "report.md").write_text(markdown, encoding="utf-8")
    sys.stdout.write(markdown)
    totals = report["totals"]
    return 1 if totals["failed"] or totals["errored"] else 0


if __name__ == "__main__":
    sys.exit(main())
