import json

import harness
import report


def test_to_anthropic_tool_maps_fields_and_restates_removed_alternatives():
    tool = {
        "name": "occupancy",
        "description": "Physical occupancy.",
        "inputSchema": {
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "properties": {"revision_id": {"type": "string"}, "unit_count": {"type": "integer"}},
            "oneOf": [
                {"required": ["revision_id"], "properties": {"snapshot": False}},
                {"required": ["snapshot", "unit_count"], "properties": {"revision_id": False}},
            ],
        },
    }
    converted = harness.to_anthropic_tool(tool)
    assert set(converted) == {"name", "description", "input_schema"}
    assert converted["name"] == "occupancy"
    schema = converted["input_schema"]
    assert "$schema" not in schema and "oneOf" not in schema
    assert schema["type"] == "object" and "revision_id" in schema["properties"]
    assert "revision_id | snapshot and unit_count" in converted["description"]
    assert "oneOf" in tool["inputSchema"], "the MCP tool is not mutated"


def test_plain_schemas_pass_through_unchanged():
    tool = {"name": "t12", "description": "d", "inputSchema": {"type": "object", "properties": {"a": {"type": "string"}}}}
    converted = harness.to_anthropic_tool(tool)
    assert converted == {"name": "t12", "description": "d", "input_schema": tool["inputSchema"]}


def test_report_totals_and_markdown_over_fixtures(fixture):
    names = ["grounded_noi", "invented_number", "derived_percentage", "no_citation"]
    transcripts = [fixture(n) for n in names]
    transcripts[0]["usage"] = {"input_tokens": 1200, "output_tokens": 300, "total_tokens": 1500}
    built = report.build_report(transcripts)
    totals = built["totals"]
    assert totals["scenarios"] == 4 and totals["passed"] == 1 and totals["failed"] == 3
    assert totals["tokens"] == 1500 and totals["input_tokens"] == 1200
    assert totals["failing_by_check"]["grounding"] == 1
    assert totals["failing_by_check"]["no_arithmetic"] == 1
    assert totals["invented_numbers"] == 2  # 14,250.00 and the derived 27.13%
    markdown = report.render_markdown(built)
    lines = markdown.splitlines()
    assert lines[0] == "| scenario | result | failing checks | invented numbers | tokens |"
    assert any("| FAIL | grounding, must_mention | 14,250.00 |" in line for line in lines)
    assert any("| pass | - | - | 1500 |" in line for line in lines)
    json.dumps(built)  # serialisable


def test_report_marks_dry_run_and_errored_transcripts():
    dry = {"scenario_id": "a", "title": "A", "dry_run": True, "usage": {"total_tokens": 0}}
    errored = {"scenario_id": "b", "title": "B", "dry_run": False, "error": "HarnessError: boom",
               "scenario": {"id": "b", "question": "q", "expect": {}}, "answer": None, "tool_calls": []}
    built = report.build_report([dry, errored])
    assert built["totals"]["dry_run"] == 1 and built["totals"]["errored"] == 1
    markdown = report.render_markdown(built)
    assert "| a | dry run |" in markdown and "| b | ERROR | HarnessError: boom |" in markdown
