"""Focused integration: long records, parallel branches, local failure recovery and provenance."""
from __future__ import annotations

from collections import Counter
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import subprocess
import sys
import tempfile
from threading import Lock, Thread
import time


def main():
    counts, failed, malformed, limited = Counter(), False, False, False
    active = peak = 0
    department_sources = set()
    guard = Lock()
    expected = "## 当日任务\n讨论了发布；修正后仅完成静态检查，尚未验证上线。"

    class Provider(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_POST(self):
            nonlocal failed, malformed, limited, active, peak
            request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            prompt = request["messages"][0]["content"]
            payload = json.loads(request["messages"][1]["content"])
            assert "max_tokens" not in request and "max_completion_tokens" not in request
            segments = [s for s in payload.get("segments", []) if not s.get("contextOnly")]
            key = prompt + ":" + ",".join(s["ref"] for s in segments)
            with guard:
                counts[key] += 1
                active += 1
                peak = max(peak, active)
            try:
                time.sleep(0.08)
                if prompt == "FINAL" and "events" in payload or prompt == "MERGE" and any(len(s["text"]) > 3000 for s in payload.get("sourceContext", [])):
                    self.send_response(400)
                    self.send_header("Content-Type", "application/json")
                    self.end_headers()
                    self.wfile.write(json.dumps({"error": {"code": "context_length_exceeded", "message": "maximum context length"}}).encode())
                    return
                if prompt == "EXTRACT" and payload["scope"] == "session-A" and len(segments) > 1:
                    self.send_response(400)
                    self.send_header("Content-Type", "application/json")
                    self.end_headers()
                    self.wfile.write(json.dumps({"error": {"code": "context_length_exceeded", "message": "maximum context length"}}).encode())
                    return
                if prompt == "EXTRACT" and payload["scope"] == "session-B" and not failed:
                    failed = True
                    self.send_response(401)
                    self.send_header("Content-Type", "application/json")
                    self.end_headers()
                    self.wfile.write(json.dumps({"error": {"code": "invalid_api_key", "message": "fixture one-shot authentication failure"}}).encode())
                    return
                if prompt == "EXTRACT" and payload["scope"] == "session-C" and not limited:
                    limited = True
                    self.send_response(429)
                    self.send_header("Content-Type", "application/json")
                    self.send_header("Retry-After", "0")
                    self.end_headers()
                    self.wfile.write(json.dumps({"error": "concurrent request limit"}).encode())
                    return
                if prompt == "EXTRACT":
                    facts = [{"text": "修正：只完成静态检查，未验证上线" if "corrected" in s["text"] else "讨论发布方案", "refs": [s["ref"]]} for s in segments]
                    if payload["kind"] == "department":
                        facts = []
                        for segment in segments:
                            expected_author = {"r1": "甲", "r2": "乙"}[segment["reportId"]]
                            assert segment["author"] == expected_author and segment["version"] == 3
                            personal = segment["kind"] == "supplement"
                            if personal:
                                assert segment["personalStatement"] and "成员原话" in segment["text"]
                            with guard:
                                department_sources.add((segment["reportId"], segment["kind"]))
                            facts.append({"text": f"{expected_author} V3 " + ("成员陈述：成员原话" if personal else "记录正文"), "refs": [segment["ref"]]})
                    answer = {"facts": facts}
                    if payload["scope"] == "session-C" and not malformed:
                        malformed = True
                        answer = {"facts": [{"text": "bad ref", "refs": ["not-in-source"]}]}
                elif prompt == "MERGE":
                    answer = {"facts": payload["facts"]}
                    if payload["kind"] != "department" and not payload.get("contextReview"):
                        answer["facts"] = []
                        answer["needsContext"] = [payload["facts"][0]["refs"][0]]
                    elif payload.get("sourceContext"):
                        assert payload["sourceContext"][0]["ref"] in payload["facts"][0]["refs"]
                else:
                    if "reports" in payload:
                        raise AssertionError("Department fixture should use segmented path")
                    if "department" in payload:
                        for author in ("甲", "乙"):
                            assert any(f"{author} V3 成员陈述" in f["text"] for f in payload["evidence"])
                    else:
                        assert any("修正" in f["text"] for f in payload["evidence"])
                    answer = expected
                output = answer if isinstance(answer, str) else json.dumps(answer, ensure_ascii=False)
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream; charset=utf-8")
                self.end_headers()
                event = {"id": "fixture", "object": "chat.completion.chunk", "created": 1, "model": "fixture", "choices": [{"index": 0, "delta": {"role": "assistant", "content": output}, "finish_reason": "stop"}]}
                self.wfile.write(("data: " + json.dumps(event, ensure_ascii=False) + "\n\ndata: [DONE]\n\n").encode())
                self.wfile.flush()
            finally:
                with guard:
                    active -= 1

    server = ThreadingHTTPServer(("127.0.0.1", 0), Provider)
    Thread(target=server.serve_forever, daemon=True).start()
    command = [sys.argv[1]] if len(sys.argv) > 1 else [sys.executable, str(Path(__file__).with_name("runner.py"))]
    try:
        with tempfile.TemporaryDirectory(prefix="report-pipeline-") as directory:
            events = [{"id": f"{session}-{i}", "sessionId": "session-" + session,
                "timestamp": f"2026-11-09T0{i}:00:00+08:00", "kind": "user" if i == 0 else "tool_result",
                "source": f"fixture-{session}.jsonl:{i}", "text": ("corrected " if i == 3 else "proposal ") + "sample output value " * 500}
                for session in "ABC" for i in range(4)]
            request = {"jobId": "long-fixture", "kind": "personal", "systemPrompt": "FINAL",
                "prompts": {"extract": "EXTRACT", "merge": "MERGE"}, "input": {"events": events},
                "api": {"baseUrl": f"http://127.0.0.1:{server.server_port}/v1", "key": "fixture-key", "model": "fixture",
                    "contextTokens": 30000, "outputReserveTokens": 1000, "chunkTokens": 6500}, "workspace": directory}

            def run():
                result = subprocess.run(command, input=json.dumps(request), text=True, encoding="utf-8", capture_output=True, timeout=90)
                rows = [json.loads(line) for line in result.stdout.splitlines()]
                assert rows, result.stderr
                return result, rows

            first, rows = run()
            assert first.returncode == 1 and rows[-1]["type"] == "error", first.stdout + first.stderr
            before = counts.copy()
            second, rows = run()
            assert second.returncode == 0, second.stdout[-12000:] + second.stderr
            assert rows[-1] == {"type": "result", "text": expected}
            evidence = next(row for row in rows if row["type"] == "evidence")
            assert evidence["coverage"]["complete"] and evidence["coverage"]["mode"] == "staged"
            assert len(evidence["coverage"]["segments"]) >= 6
            assert all(span["ref"] in evidence["sources"] for piece in evidence["coverage"]["segments"] for span in piece["ranges"])
            assert peak > 1 and peak <= 4, peak
            # Successful branches and remembered overflow parents must not be sent again.
            for key, count in before.items():
                if key.startswith("EXTRACT:") and "B-" not in key:
                    assert counts[key] == count, (key, count, counts[key])
            for original in events:
                ranges = sorted((span["start"], span["end"]) for piece in evidence["coverage"]["segments"] for span in piece["ranges"] if span["eventId"] == original["id"])
                assert ranges and ranges[0][0] == 0 and ranges[-1][1] == len(original["text"]), (original["id"], ranges)
                assert all(left[1] == right[0] for left, right in zip(ranges, ranges[1:])), ranges
            previous = counts.copy()
            third, rows = run()
            assert third.returncode == 0 and rows[-1]["text"] == expected
            assert counts == previous
            # Simulate exit after graph completion but before final/evidence cache publication.
            next(Path(directory).glob("pipeline/*/final.json")).unlink()
            next(Path(directory).glob("pipeline/*/evidence.json")).unlink()
            recovered, rows = run()
            assert recovered.returncode == 0 and counts == previous
            restored = next(row for row in rows if row["type"] == "evidence")
            assert restored["sources"] == evidence["sources"]
            print(f"PASS parallel peak={peak}; all source spans covered; overflow split, local failure resume, ref repair and completed cache")
            request["kind"] = "department"
            request["input"] = {"department": "隔离部门", "day": "2026-11-09", "reports": [
                {"id": rid, "memberName": author, "version": 3, "body": "work item output " * 6000,
                 "supplement": {"author": author, "text": "成员原话：仍需现场确认", "submittedAt": "2026-11-09T09:00:00Z"}}
                for rid, author in (("r1", "甲"), ("r2", "乙"))]}
            department, rows = run()
            assert department.returncode == 0, department.stdout[-12000:] + department.stderr
            assert department_sources == {(rid, kind) for rid in ("r1", "r2") for kind in ("report", "supplement")}
            coverage = next(row["coverage"] for row in rows if row["type"] == "evidence")
            assert coverage["complete"] and coverage["inputReports"] == 2
            print("PASS department author/report version and original supplement attribution through segmented path")
    finally:
        server.shutdown()
        server.server_close()


if __name__ == "__main__":
    main()
