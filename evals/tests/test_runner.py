"""The runner's tool-use loop, driven by a scripted fake client against the real
`boxscore-exact mcp` binary. Skipped when the binary is not built (CI builds no
release binary for the evals job). No network, no key, no SDK."""
import json
from types import SimpleNamespace

import pytest

import harness
import run
import score

try:
    BINARY = harness.find_binary()
except harness.HarnessError:
    BINARY = None

needs_binary = pytest.mark.skipif(BINARY is None, reason="boxscore-exact release binary not built")


class Block(SimpleNamespace):
    def model_dump(self, exclude_none=True):
        return dict(vars(self))


def response(blocks, stop_reason, tokens=(100, 20)):
    return SimpleNamespace(
        content=blocks, stop_reason=stop_reason,
        usage=SimpleNamespace(input_tokens=tokens[0], output_tokens=tokens[1],
                              cache_creation_input_tokens=0, cache_read_input_tokens=0),
    )


def tool_use(i, name, **arguments):
    return Block(type="tool_use", id=f"toolu_{i}", name=name, input=arguments)


class ScriptedClient:
    """A model that lists revisions, reviews the first one, then answers from the result."""

    def __init__(self):
        self.requests = []
        self.messages = SimpleNamespace(create=self.create)

    def create(self, **kwargs):
        self.requests.append(kwargs)
        turn = len(self.requests)
        messages = kwargs["messages"]
        if turn == 1:
            return response([tool_use(1, "list_revisions")], "tool_use")
        last = json.loads(messages[-1]["content"][0]["content"])
        if turn == 2:
            return response([tool_use(2, "review_period", revision_id=last["result"]["revisions"][0]["id"])], "tool_use")
        e = last
        cite = f"{e['function']}@{e['version']}, trace {e['trace_id']}"
        bridge = e["result"]["variance"]["noi_bridge"]
        text = (
            f"NOI was {bridge['actual_noi']} against a budget of {bridge['budget_noi']} ({cite}).\n"
            f"- The miss is {bridge['noi_variance']} ({cite}).\n"
            f"- Revenue variance {bridge['revenue_variance']}, expense variance {bridge['expense_variance']} ({cite})."
        )
        return response([Block(type="text", text=text)], "end_turn")


def scenario(scenario_id):
    return next(s for s in harness.load_scenarios() if s["id"] == scenario_id)


@needs_binary
def test_tool_use_loop_records_a_transcript_the_scorer_passes():
    client = ScriptedClient()
    usage = run.Usage()
    transcript = run.run_scenario(
        scenario("noi_miss_budget_may"), binary=BINARY, client=client, model="fake-model",
        dry_run=False, run_usage=usage, max_total_tokens=None, max_tool_calls=8,
    )
    assert transcript["error"] is None, transcript["error"]
    assert transcript["model_turns"] == 3
    assert [c["request"]["name"] for c in transcript["tool_calls"]] == ["list_revisions", "review_period"]
    for call in transcript["tool_calls"]:
        assert call["envelope"]["function"] == call["request"]["name"]
        assert json.loads(call["content_text"]) == call["envelope"]  # text and structure agree
    assert transcript["usage"]["total_tokens"] == 360 == usage.total
    # what the model was sent: the converted tools, the grounding prompt, the analyst sentence
    first = client.requests[0]
    assert len(first["tools"]) == 12 and set(first["tools"][0]) == {"name", "description", "input_schema"}
    assert first["system"].startswith("Numbers come from the engine.")
    assert first["system"].endswith(harness.ANALYST_SENTENCE)
    assert first["max_tokens"] == 1500
    assert first["messages"][0] == {"role": "user", "content": "Why did NOI miss budget for Sample Gardens in 2026-05?"}
    assert transcript["mcp"]["exit_code"] == 0 and transcript["mcp"]["stderr"] == ""
    result = score.score_transcript(transcript)
    assert result["passed"], result["checks"]


@needs_binary
def test_token_budget_stops_the_scenario_and_records_it():
    transcript = run.run_scenario(
        scenario("noi_miss_budget_may"), binary=BINARY, client=ScriptedClient(), model="fake-model",
        dry_run=False, run_usage=run.Usage(), max_total_tokens=100, max_tool_calls=8,
    )
    assert transcript["error"].startswith("budget_exceeded")
    assert transcript["model_turns"] == 1


@needs_binary
def test_tool_call_cap_is_enforced():
    class Greedy:
        def __init__(self):
            self.messages = SimpleNamespace(create=self.create)

        def create(self, **kwargs):
            return response([tool_use(len(kwargs["messages"]), "list_revisions")], "tool_use")

    transcript = run.run_scenario(
        scenario("what_is_loaded"), binary=BINARY, client=Greedy(), model="fake-model",
        dry_run=False, run_usage=run.Usage(), max_total_tokens=None, max_tool_calls=2,
    )
    assert len(transcript["tool_calls"]) == 2
    assert transcript["tool_call_cap_hit"] is True


@needs_binary
def test_dry_run_records_tools_and_prompt_with_zero_model_turns(tmp_path, capsys):
    code = run.main(["--dry-run", "--scenario", "occupancy", "--out", str(tmp_path)])
    assert code == 0
    (run_dir,) = tmp_path.iterdir()
    assert run_dir.name.endswith("-dry-run")
    transcript = json.loads((run_dir / "occupancy.json").read_text())
    assert transcript["dry_run"] is True and transcript["model_turns"] == 0 and transcript["tool_calls"] == []
    assert len(transcript["mcp"]["tools"]) == 12
    assert transcript["system_prompt"].startswith("Numbers come from the engine.")
    assert transcript["setup"]["aliases"]["may"]


def test_without_a_key_a_live_run_exits_2_and_does_not_prepare_anything(monkeypatch, tmp_path, capsys):
    monkeypatch.delenv("ANTHROPIC_API_KEY", raising=False)
    assert run.main(["--out", str(tmp_path)]) == 2
    assert "ANTHROPIC_API_KEY is not set" in capsys.readouterr().err
    assert list(tmp_path.iterdir()) == []


def test_unknown_scenario_id_is_a_usage_error(tmp_path, capsys):
    if BINARY is None:
        pytest.skip("boxscore-exact release binary not built")
    assert run.main(["--dry-run", "--scenario", "nope", "--out", str(tmp_path)]) == 2
