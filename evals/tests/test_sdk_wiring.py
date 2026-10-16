"""The runner against the real `anthropic` SDK pointed at a canned-response
HTTP server on 127.0.0.1 (stdlib only): checks the request body the SDK sends
and that real SDK response objects round-trip through the loop. Skipped when
`anthropic` or the engine binary is absent. Nothing leaves the machine."""
import json
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer

import pytest

anthropic = pytest.importorskip("anthropic")

import harness  # noqa: E402
import run  # noqa: E402
import score  # noqa: E402

try:
    BINARY = harness.find_binary()
except harness.HarnessError:
    BINARY = None


def message_json(content, stop_reason):
    return {
        "id": "msg_test", "type": "message", "role": "assistant", "model": "claude-sonnet-5-5",
        "content": content, "stop_reason": stop_reason, "stop_sequence": None,
        "usage": {"input_tokens": 50, "output_tokens": 10},
    }


@pytest.mark.skipif(BINARY is None, reason="boxscore-exact release binary not built")
def test_request_bodies_and_response_parsing_with_the_real_sdk():
    bodies = []

    def reply(body):
        bodies.append(body)
        if len(bodies) == 1:
            return message_json(
                [{"type": "text", "text": "Checking."},
                 {"type": "tool_use", "id": "toolu_1", "name": "list_revisions", "input": {}}], "tool_use")
        result = json.loads(body["messages"][-1]["content"][0]["content"])
        cite = f"list_revisions@1.0.0, trace {result['trace_id']}"
        return message_json(
            [{"type": "text", "text": f"Sample Gardens 2026-05 has 24 units ({cite})."}], "end_turn")

    class Handler(BaseHTTPRequestHandler):
        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            payload = json.dumps(reply(body)).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

        def log_message(self, *args):
            pass

    server = HTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        client = anthropic.Anthropic(api_key="not-a-real-key", base_url=f"http://127.0.0.1:{server.server_port}", max_retries=0)
        scenario = next(s for s in harness.load_scenarios() if s["id"] == "unit_count")
        transcript = run.run_scenario(
            scenario, binary=BINARY, client=client, model="claude-sonnet-5-5", dry_run=False,
            run_usage=run.Usage(), max_total_tokens=None, max_tool_calls=8,
        )
    finally:
        server.shutdown()
    assert transcript["error"] is None, transcript["error"]
    first, second = bodies
    assert first["model"] == "claude-sonnet-5-5" and first["max_tokens"] == 1024
    assert first["system"].startswith("Numbers come from the engine.")
    assert {t["name"] for t in first["tools"]} >= {"review_period", "list_revisions", "t12"}
    for tool in first["tools"]:
        assert set(tool) == {"name", "description", "input_schema"}
        assert tool["input_schema"]["type"] == "object"
        assert not {"oneOf", "anyOf", "allOf", "$schema"} & set(tool["input_schema"])
    # the assistant turn goes back as plain content blocks, then a tool_result for the same id
    assert [m["role"] for m in second["messages"]] == ["user", "assistant", "user"]
    assert second["messages"][1]["content"][1]["type"] == "tool_use"
    tool_result = second["messages"][2]["content"][0]
    assert tool_result["type"] == "tool_result" and tool_result["tool_use_id"] == "toolu_1"
    assert tool_result["is_error"] is False
    assert transcript["usage"]["input_tokens"] == 100 and transcript["usage"]["output_tokens"] == 20
    assert score.score_transcript(transcript)["passed"]
