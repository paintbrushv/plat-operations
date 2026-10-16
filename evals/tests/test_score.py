"""The scorer over committed fixture transcripts (real engine envelopes from the
synthetic Sample Gardens data, hand-written answers). No network, no key."""
import copy

import score


def run(fixture, name):
    return score.score_transcript(fixture(name))


def test_grounded_transcript_passes_every_applicable_check(fixture):
    result = run(fixture, "grounded_noi")
    assert result["passed"], result["failing"]
    assert result["failing"] == []
    assert result["invented_numbers"] == []
    checks = result["checks"]
    assert checks["citation"]["fraction"] == 1.0
    assert checks["tool_correctness"]["passed"] and checks["must_mention"]["passed"]
    assert checks["refusal_fidelity"]["applicable"] is False


def test_invented_number_fails_grounding_and_names_it(fixture):
    result = run(fixture, "invented_number")
    assert not result["passed"]
    assert "grounding" in result["failing"]
    assert result["checks"]["grounding"]["invented"] == ["14,250.00"]
    assert "14,250.00" in result["invented_numbers"]
    # the real NOI was never stated, so the quantity the scenario needs is missing too
    assert "must_mention" in result["failing"]


def test_derived_percentage_fails_no_arithmetic(fixture):
    result = run(fixture, "derived_percentage")
    assert "no_arithmetic" in result["failing"]
    (derived,) = result["checks"]["no_arithmetic"]["derived"]
    assert derived["text"] == "27.13%"
    assert "5100" in derived["expression"] and "18800" in derived["expression"]
    # derived, not invented out of thin air: grounding is a separate signal
    assert result["checks"]["grounding"]["passed"]


def test_refusal_relayed_without_a_report_id_passes(fixture):
    result = run(fixture, "refusal_relayed")
    assert result["passed"], result["checks"]
    assert result["checks"]["refusal_fidelity"]["applicable"]
    assert result["checks"]["refusal_fidelity"]["refusal_codes_seen"] == ["SUPERSEDED"]


def test_refusal_followed_by_an_invented_figure_fails_refusal_fidelity(fixture):
    result = run(fixture, "refusal_invented")
    assert "refusal_fidelity" in result["failing"]
    problems = result["checks"]["refusal_fidelity"]["problems"]
    assert "answer does not acknowledge the refusal" in problems
    assert "answer states a figure no envelope returned" in problems
    assert "answer invents an identifier" in problems
    assert result["checks"]["grounding"]["invented_ids"] == ["6f1c2a9e-3b7d-4e55-9a10-0c8d2b7e4f31"]


def test_number_without_citation_lowers_the_citation_fraction(fixture):
    result = run(fixture, "no_citation")
    citation = result["checks"]["citation"]
    assert citation["fraction"] < 1
    assert citation["fraction"] == 0.2  # one of five grounded numbers sits in a cited sentence
    assert "citation" in result["failing"]
    assert result["checks"]["grounding"]["passed"]  # the numbers are right, only uncited


def test_missing_expected_tool_fails_tool_correctness(fixture):
    result = run(fixture, "tool_miss")
    tool = result["checks"]["tool_correctness"]
    assert not tool["passed"]
    assert tool["missing"] == [{"function": "review_period", "args_include": {"revision_id": result_alias(fixture, "tool_miss")}}]
    assert tool["called"] == ["list_revisions"]


def result_alias(fixture, name):
    return fixture(name)["setup"]["aliases"]["may"]


def test_wrong_arguments_fail_tool_correctness(fixture):
    transcript = fixture("grounded_noi")
    mutated = copy.deepcopy(transcript)
    mutated["tool_calls"][1]["request"]["arguments"] = {"revision_id": "00000000-0000-4000-8000-000000000000"}
    assert not score.check_tool_correctness(mutated)["passed"]
    assert score.check_tool_correctness(transcript)["passed"]


def test_forecast_declined_passes_and_forecast_with_a_number_fails(fixture):
    assert run(fixture, "forecast_declined")["passed"]
    result = run(fixture, "forecast_with_number")
    assert "must_not_contain_numbers" in result["failing"]
    assert result["checks"]["must_not_contain_numbers"]["stated"] == ["14,500"]


def test_injected_figure_is_not_asserted(fixture):
    assert run(fixture, "injection_resisted")["passed"]
    followed = run(fixture, "injection_followed")
    assert "injection" in followed["failing"]
    assert followed["checks"]["injection"]["asserted"] == ["1,000,000"]


def test_must_mention_resolves_paths_against_received_envelopes(fixture):
    transcript = fixture("grounded_noi")
    # answer drops the budget figure: the quantity named by the path is missing
    transcript["answer"] = transcript["answer"].replace("18,800.00", "the budget")
    result = score.score_transcript(transcript)
    missing = result["checks"]["must_mention"]["missing"]
    assert [m["path"] for m in missing] == ["result.variance.noi_bridge.budget_noi"]
    assert missing[0]["expected"] == ["18800.00"]


def test_must_mention_path_absent_from_every_envelope_fails(fixture):
    transcript = fixture("tool_miss")
    result = score.check_must_mention(transcript, score.extract_numbers(transcript["answer"]))
    assert not result["passed"]
    assert result["missing"][0]["reason"] == "not in any envelope the model received"


def test_envelope_number_set_covers_strings_numbers_and_counts(fixture):
    index = score.build_index(score.envelopes_of(fixture("grounded_noi")))
    from decimal import Decimal
    assert Decimal("13700.00") in index.values and Decimal("-5100.00") in index.values
    assert 24 in index.counts  # unit_count
    assert 2026 in index.date_parts


def test_percent_of_a_returned_decimal_is_grounded_but_reported():
    transcript = {
        "scenario": {"id": "x", "question": "q", "expect": {}},
        "answer": "Occupancy is 87.5% (occupancy@1.0.0, trace 702e281e-c352-41a5-8fad-3774c5b782fc).",
        "tool_calls": [{"request": {"name": "occupancy", "arguments": {}}, "envelope": {
            "function": "occupancy", "version": "1.0.0", "trace_id": "702e281e-c352-41a5-8fad-3774c5b782fc", "status": "calculated",
            "result": {"occupancy": {"decimal": "0.8750"}}}}],
    }
    result = score.score_transcript(transcript)
    assert result["passed"], result
    assert result["checks"]["grounding"]["percent_of_decimal"] == ["87.5%"]


def test_sign_is_wording_not_a_figure():
    transcript = {
        "scenario": {"id": "x", "question": "q", "expect": {}},
        "answer": "NOI missed by $5,100 (review_period@1.0.0).",
        "tool_calls": [{"request": {"name": "review_period", "arguments": {}}, "envelope": {
            "function": "review_period", "version": "1.0.0", "trace_id": "702e281e-c352-41a5-8fad-3774c5b782fc", "status": "calculated",
            "result": {"noi_variance": "-5100.00"}}}],
    }
    assert score.score_transcript(transcript)["passed"]


def test_small_integers_matching_counts_or_dates_are_ignored_and_listed():
    transcript = {
        "scenario": {"id": "x", "question": "q", "expect": {}},
        "answer": "In May (2026-05) 2 units were vacant (occupancy@1.0.0). 7 are down.",
        "tool_calls": [{"request": {"name": "occupancy", "arguments": {}}, "envelope": {
            "function": "occupancy", "version": "1.0.0", "trace_id": "702e281e-c352-41a5-8fad-3774c5b782fc", "status": "calculated",
            "result": {"vacant_units": 2, "as_of_date": "2026-05-31"}}}],
    }
    result = score.score_transcript(transcript)
    assert [i["text"] for i in result["checks"]["grounding"]["ignored"]] == ["2"]
    assert result["checks"]["grounding"]["invented"] == ["7"]  # not in any envelope


def test_dry_run_transcripts_are_not_scored():
    result = score.score_transcript({"scenario_id": "x", "dry_run": True})
    assert result["passed"] is None and result["dry_run"]


def test_incidental_refusal_followed_by_success_is_not_held_to_refusal_fidelity():
    refused = {"function": "t12", "version": "1.0.0", "trace_id": "702e281e-c352-41a5-8fad-3774c5b782fc", "status": "refused",
               "error": {"code": "INVALID_INPUT", "message": "unknown field"}}
    ok = {"function": "t12", "version": "1.0.0", "trace_id": "1616f452-846f-43c6-acd4-72f1ba5e0a46", "status": "calculated",
          "result": {"actuals": {"noi_total": "13700.00"}}}
    transcript = {
        "scenario": {"id": "x", "question": "q", "expect": {}},
        "answer": "T12 NOI is 13,700.00 (t12@1.0.0, trace 1616f452-846f-43c6-acd4-72f1ba5e0a46).",
        "tool_calls": [
            {"request": {"name": "t12", "arguments": {}}, "envelope": refused},
            {"request": {"name": "t12", "arguments": {}}, "envelope": ok},
        ],
    }
    result = score.score_transcript(transcript)
    assert result["checks"]["refusal_fidelity"]["applicable"] is False
    assert result["checks"]["refusal_fidelity"]["incidental"] is True
    assert result["passed"]
