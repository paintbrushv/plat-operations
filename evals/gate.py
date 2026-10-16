#!/usr/bin/env python3
"""Apply the thresholds to a scored eval run and record the result.

    python3 evals/gate.py evals/out/<timestamp>            # a run.py directory
    python3 evals/gate.py evals/out/<timestamp>/report.json  # report.py's JSON

Reads the transcripts (or report.json), scores them with report.py, compares
the totals with evals/thresholds.json, prints a markdown summary to stdout (and
appends it to $GITHUB_STEP_SUMMARY when that is set), appends one JSON line to
the trend file, and exits 1 when the run fails. Standard library only: no SDK,
no network, no key.

A run fails when any minimum is missed, when any scenario fails a hard-fail
check, or when any scenario errored (the run is incomplete). A run with no
scored scenario (a dry run, an empty directory) also fails: there is nothing to
vouch for. Exit status: 0 passed, 1 failed, 2 usage or input error.
"""
from __future__ import annotations

import argparse
import json
import os
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

import report as report_module

EVALS_DIR = Path(__file__).resolve().parent
DEFAULT_THRESHOLDS = EVALS_DIR / "thresholds.json"
DEFAULT_TREND = EVALS_DIR / "trend.jsonl"

# Each minimum names the check it reads and how: a pass rate over the scenarios
# the check applied to, or the mean of the citation fraction.
MINIMUMS = {
    "grounding_pass_rate": ("grounding", "rate"),
    "refusal_fidelity_pass_rate": ("refusal_fidelity", "rate"),
    "no_arithmetic_pass_rate": ("no_arithmetic", "rate"),
    "tool_correctness_pass_rate": ("tool_correctness", "rate"),
    "citation_fraction_mean": ("citation", "mean"),
    "overall_pass_rate": (None, "overall"),
}
# Checks counted per scenario in the trend line, in a stable order.
TREND_CHECKS = (
    "grounding", "no_arithmetic", "citation", "refusal_fidelity",
    "tool_correctness", "must_mention", "injection", "text",
    "must_not_contain_numbers", "cite_traces",
)


class GateError(Exception):
    pass


def load_thresholds(path: Path) -> dict[str, Any]:
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise GateError(f"cannot read thresholds {path}: {exc}") from exc
    minimums = data.get("minimums")
    hard = data.get("hard_fail_checks")
    if not isinstance(minimums, dict) or not isinstance(hard, list):
        raise GateError(f"{path}: expected an object with 'minimums' (object) and 'hard_fail_checks' (list)")
    unknown = sorted(set(minimums) - set(MINIMUMS))
    if unknown:
        raise GateError(f"{path}: unknown minimum(s) {unknown}; known: {sorted(MINIMUMS)}")
    for name, value in minimums.items():
        if isinstance(value, bool) or not isinstance(value, (int, float)) or not 0 <= value <= 1:
            raise GateError(f"{path}: minimum {name} must be a number from 0 to 1, got {value!r}")
    if not all(isinstance(c, str) for c in hard):
        raise GateError(f"{path}: hard_fail_checks must be a list of check names")
    return data


def load_report(path: Path) -> tuple[dict[str, Any], str, str | None]:
    """The report dict, the run id and the model named by `_run.json`, if any."""
    if path.is_dir():
        run_dir = path
        built = report_module.build_report(report_module.load_transcripts(path))
    elif path.is_file():
        run_dir = path.parent
        try:
            built = json.loads(path.read_text(encoding="utf-8"))
        except json.JSONDecodeError as exc:
            raise GateError(f"{path} is not JSON: {exc}") from exc
        if not isinstance(built, dict) or "totals" not in built or "scenarios" not in built:
            raise GateError(f"{path} is not a report.py JSON file (no 'totals' and 'scenarios')")
    else:
        raise GateError(f"no such file or directory: {path}")
    run_model = None
    run_file = run_dir / "_run.json"
    if run_file.is_file():
        try:
            run_model = json.loads(run_file.read_text(encoding="utf-8")).get("model")
        except json.JSONDecodeError:
            pass
    return built, run_dir.resolve().name, run_model


def check_counts(scored: list[dict[str, Any]]) -> dict[str, dict[str, int]]:
    """Per check: scenarios it applied to and how many passed."""
    counts: dict[str, dict[str, int]] = {}
    for row in scored:
        for name, check in (row.get("checks") or {}).items():
            if not check.get("applicable"):
                continue
            entry = counts.setdefault(name, {"applicable": 0, "passed": 0})
            entry["applicable"] += 1
            entry["passed"] += 1 if check.get("passed") else 0
    return counts


def citation_mean(scored: list[dict[str, Any]]) -> float | None:
    fractions = []
    for row in scored:
        check = (row.get("checks") or {}).get("citation") or {}
        if check.get("applicable") and check.get("fraction") is not None:
            fractions.append(float(check["fraction"]))
    return sum(fractions) / len(fractions) if fractions else None


def evaluate(built: dict[str, Any], thresholds: dict[str, Any]) -> dict[str, Any]:
    rows = built["scenarios"]
    scored = [r for r in rows if not r.get("dry_run")]
    counts = check_counts(scored)
    mean = citation_mean(scored)
    passed_count = sum(1 for r in scored if r.get("passed") and not r.get("errored"))
    errored = [r["scenario_id"] for r in scored if r.get("errored")]

    rows_out = []
    reasons = []
    for name, minimum in thresholds["minimums"].items():
        check, kind = MINIMUMS[name]
        if kind == "overall":
            value = passed_count / len(scored) if scored else None
        elif kind == "mean":
            value = mean
        else:
            c = counts.get(check)
            value = c["passed"] / c["applicable"] if c else None
        # A check that applied to no scenario in this run has nothing to measure
        # (an empty run is refused separately, below).
        ok = value is None or value >= minimum
        rows_out.append({"name": name, "minimum": minimum, "value": value, "ok": ok})
        if not ok:
            reasons.append(f"{name} {_fmt(value)} is below the minimum {_fmt(minimum)}")

    hard = set(thresholds["hard_fail_checks"])
    hard_failures = []
    for row in scored:
        for name in row.get("failing") or []:
            if name in hard:
                hard_failures.append({"scenario_id": row["scenario_id"], "check": name})
    for item in hard_failures:
        reasons.append(f"hard fail: {item['scenario_id']} failed {item['check']}")
    for scenario_id in errored:
        reasons.append(f"scenario {scenario_id} errored; the run is incomplete")
    if not scored:
        reasons.append("no scored scenario in this run (a dry run or an empty directory)")
    return {
        "passed": not reasons,
        "reasons": reasons,
        "rows": rows_out,
        "hard_failures": hard_failures,
        "errored": errored,
        "scenarios": len(scored),
        "scenarios_passed": passed_count,
        "counts": counts,
        "citation_mean": mean,
        "tokens": sum(r.get("tokens", 0) for r in rows),
    }


def _fmt(value: float | None) -> str:
    return "n/a" if value is None else f"{value:.3g}"


def render_markdown(outcome: dict[str, Any], built: dict[str, Any], run_id: str, model: str) -> str:
    lines = [
        f"## Agent grounding evals: {'PASSED' if outcome['passed'] else 'FAILED'}",
        "",
        f"Run `{run_id}`, model `{model}`, {outcome['scenarios_passed']} of {outcome['scenarios']} "
        f"scenario(s) passed, {outcome['tokens']} tokens.",
        "",
        "| threshold | minimum | this run | result |",
        "|---|---|---|---|",
    ]
    for row in outcome["rows"]:
        lines.append(f"| {row['name']} | {_fmt(row['minimum'])} | {_fmt(row['value'])} | "
                     f"{'ok' if row['ok'] else 'BELOW'} |")
    lines.append("")
    if outcome["reasons"]:
        lines += ["Why it failed:", ""] + [f"- {reason}" for reason in outcome["reasons"]] + [""]
    failed = [r for r in built["scenarios"] if not r.get("dry_run") and (r.get("failing") or r.get("errored"))]
    if failed:
        lines += ["| scenario | failing checks | invented numbers |", "|---|---|---|"]
        for row in failed:
            failing = (row.get("error") or "errored")[:80] if row.get("errored") else ", ".join(row["failing"])
            invented = ", ".join(row.get("invented_numbers") or []) or "-"
            lines.append(f"| {row['scenario_id']} | {failing} | {invented} |")
        lines.append("")
    return "\n".join(lines)


def trend_line(outcome: dict[str, Any], run_id: str, model: str, date: str) -> dict[str, Any]:
    checks = {name: dict(outcome["counts"][name]) for name in TREND_CHECKS if name in outcome["counts"]}
    mean = outcome["citation_mean"]
    return {
        "date": date,
        "model": model,
        "run_id": run_id,
        "scenarios": outcome["scenarios"],
        "checks": checks,
        "citation_mean": None if mean is None else round(mean, 4),
        "total_tokens": outcome["tokens"],
        "passed": outcome["passed"],
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("path", type=Path, help="a run directory from run.py, or a report.json from report.py")
    parser.add_argument("--thresholds", type=Path, default=DEFAULT_THRESHOLDS)
    parser.add_argument("--trend", type=Path, default=DEFAULT_TREND,
                        help="JSONL file to append one line to (default evals/trend.jsonl)")
    parser.add_argument("--model", default=None, help="override the model recorded in the trend line")
    parser.add_argument("--run-id", default=None, help="override the run id (default: the run directory's name)")
    parser.add_argument("--date", default=None, help="override the date, YYYY-MM-DD (default: today, UTC)")
    args = parser.parse_args(argv)
    try:
        thresholds = load_thresholds(args.thresholds)
        built, dir_name, run_model = load_report(args.path)
    except GateError as exc:
        print(f"gate: {exc}", file=sys.stderr)
        return 2

    models = built.get("models") or []
    model = args.model or run_model or (",".join(models) if models else "unknown")
    run_id = args.run_id or dir_name
    date = args.date or datetime.now(timezone.utc).strftime("%Y-%m-%d")
    outcome = evaluate(built, thresholds)

    markdown = render_markdown(outcome, built, run_id, model)
    sys.stdout.write(markdown)
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a", encoding="utf-8") as handle:
            handle.write(markdown + "\n")
    line = json.dumps(trend_line(outcome, run_id, model, date), sort_keys=False, separators=(",", ":"))
    args.trend.parent.mkdir(parents=True, exist_ok=True)
    with open(args.trend, "a", encoding="utf-8") as handle:
        handle.write(line + "\n")
    print(f"Trend line appended to {args.trend}.")
    return 0 if outcome["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
