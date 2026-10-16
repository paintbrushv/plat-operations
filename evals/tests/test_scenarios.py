"""Every committed scenario is well formed. Needs no binary."""
import json
import re

import harness

TOOLS = {
    "describe", "noi_variance", "import_period", "review_period", "issue_report",
    "list_revisions", "occupancy", "economic_occupancy", "delinquency",
    "close_readiness", "t12", "get_provenance",
}
CLI_COMMANDS = {"init", "import-csv", "import-with", "import"}


def scenarios():
    return harness.load_scenarios()


def test_between_fourteen_and_eighteen_scenarios_with_unique_ids():
    found = scenarios()
    assert 14 <= len(found) <= 18
    ids = [s["id"] for s in found]
    assert len(ids) == len(set(ids))
    for s in found:
        assert re.fullmatch(r"[a-z0-9_]+", s["id"])
        assert s["_file"].endswith(f"{s['id']}.json")


def test_required_fields_and_shapes():
    for s in scenarios():
        assert {"id", "title", "setup", "question", "expect"} <= set(s), s["id"]
        assert s["question"].strip()
        expect = s["expect"]
        assert isinstance(expect["tools"], list)
        assert isinstance(expect["must_mention"], list)
        assert isinstance(expect["must_not_contain_numbers"], bool)
        assert isinstance(expect["max_tokens"], int) and 256 <= expect["max_tokens"] <= 4096
        for spec in expect["tools"]:
            names = [spec["function"]] if isinstance(spec["function"], str) else spec["function"]
            assert set(names) <= TOOLS, (s["id"], names)
            assert isinstance(spec["args_include"], dict)
        if "refusal" in expect:
            assert re.fullmatch(r"[A-Z_]+", expect["refusal"]["code"])


def test_setup_steps_are_argument_arrays_not_shell_strings():
    for s in scenarios():
        assert s["setup"][0]["args"][0] == "init", s["id"]
        aliases = set()
        for step in s["setup"]:
            args = step["args"]
            assert isinstance(args, list) and all(isinstance(a, str) for a in args)
            assert args[0] in CLI_COMMANDS
            assert "--database" in args and args[args.index("--database") + 1] == "{db}"
            for arg in args:
                for ref in re.findall(r"\{rev\.(\w+)\}", arg):
                    assert ref in aliases, (s["id"], ref)
            if "save_revision_as" in step:
                aliases.add(step["save_revision_as"])
        # every alias a scenario's expectations use was saved by a setup step
        for ref in re.findall(r"\{rev\.(\w+)\}", json.dumps(s["expect"])):
            assert ref in aliases, (s["id"], ref)


def test_setup_uses_only_synthetic_quickstart_files_and_eval_data():
    for s in scenarios():
        for step in s["setup"]:
            for arg in step["args"]:
                if arg.startswith("{"):
                    assert arg.startswith(("{repo}/docs/quickstart/", "{evals}/data/", "{db}", "{rev.")), arg


def test_the_required_situations_are_all_covered():
    ids = {s["id"] for s in scenarios()}
    for needed in (
        "what_is_loaded", "noi_miss_budget_may", "review_required_april_no_snapshot",
        "unknown_revision_refusal", "correction_what_changed", "t12_gap_months", "occupancy",
        "economic_occupancy", "delinquency", "close_readiness_may", "prompt_injection_account_name",
        "forecast_next_quarter", "issue_report_superseded", "citation_for_number", "unit_count",
    ):
        assert needed in ids


def test_start_here_questions_use_the_start_here_wording():
    by_id = {s["id"]: s for s in scenarios()}
    assert by_id["what_is_loaded"]["question"] == "What is loaded?"
    assert by_id["noi_miss_budget_may"]["question"] == "Why did NOI miss budget for Sample Gardens in 2026-05?"
    assert by_id["review_required_april_no_snapshot"]["question"] == "What does review_required mean here?"


def test_placeholders_substitute_and_unknown_ones_fail():
    import pytest
    ctx = {"db": "/d", "repo": "/r", "evals": "/e", "tmp": "/t", "rev.may": "abc"}
    assert harness.substitute(["{repo}/x", {"k": "{rev.may}"}], ctx) == ["/r/x", {"k": "abc"}]
    with pytest.raises(harness.HarnessError):
        harness.substitute("{rev.nope}", ctx)
