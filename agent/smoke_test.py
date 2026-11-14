"""One focused runtime contract test: streaming text, full input and checkpoint reuse."""

from __future__ import annotations

from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import subprocess
import sys
import tempfile
from threading import Thread


def main() -> None:
    calls: list[dict] = []
    report = "## 一、当日任务\n完成报告运行时联调。\n\n## 二、实际执行与产出\n正文完整保留中文。"

    class Provider(BaseHTTPRequestHandler):
        def log_message(self, *_: object) -> None:
            pass

        def do_POST(self) -> None:
            payload = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            calls.append(payload)
            if payload["model"] == "permanent-fixture":
                self.send_response(401)
                self.send_header("Content-Type", "application/json")
                self.end_headers()
                self.wfile.write(json.dumps({"error": {"code": "invalid_api_key", "message": "Credential rejected by upstream tenant; key=private-key-smoke; PRIVATE_SYSTEM_PROMPT_SMOKE; input=PRIVATE_WORK_RECORD"}}).encode())
                return
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream; charset=utf-8")
            self.end_headers()
            if payload["model"] in ("stream-retry-fixture", "stream-eof-fixture"):
                first = sum(p["model"] == payload["model"] for p in calls) == 1
                chunks = ["FAILED_PARTIAL_MUST_NOT_SURVIVE"] if first else [report]
                for text in chunks:
                    chunk = {"id": f"retry-{len(calls)}", "object": "chat.completion.chunk", "created": 1, "model": "fixture", "choices": [{"index": 0, "delta": {"role": "assistant", "content": text}, "finish_reason": None if first else "stop"}]}
                    self.wfile.write(("data: " + json.dumps(chunk, ensure_ascii=False) + "\n\n").encode())
                    self.wfile.flush()
                if first and payload["model"] == "stream-retry-fixture":
                    self.wfile.write(b'data: {"error":{"message":"stream error: stream disconnected before completion: stream closed before response.completed"}}\n\n')
                elif not first:
                    self.wfile.write(b"data: [DONE]\n\n")
                self.wfile.flush()
                return
            text_result = "" if len(calls) == 1 else report
            finish_reason = {2: "length", 3: "content_filter"}.get(len(calls), "stop")
            for text, finish in [(text_result[:12], None), (text_result[12:], None), ("", finish_reason)]:
                chunk = {"id": "smoke", "object": "chat.completion.chunk", "created": 1, "model": "fixture", "choices": [{"index": 0, "delta": {"role": "assistant", "content": text}, "finish_reason": finish}]}
                self.wfile.write(("data: " + json.dumps(chunk, ensure_ascii=False) + "\n\n").encode())
                self.wfile.flush()
            self.wfile.write(b"data: [DONE]\n\n")

    server = ThreadingHTTPServer(("127.0.0.1", 0), Provider)
    thread = Thread(target=server.serve_forever, daemon=True)
    thread.start()
    command = [sys.argv[1]] if len(sys.argv) > 1 else [sys.executable, str(Path(__file__).with_name("runner.py"))]
    try:
        with tempfile.TemporaryDirectory(prefix="report-agent-smoke-") as temp:
            request = {
                "jobId": "smoke-report",
                "kind": "personal",
                "systemPrompt": "PRIVATE_SYSTEM_PROMPT_SMOKE",
                "input": {"events": [{"id": "short-record", "sessionId": "fixture", "timestamp": "2026-11-09T09:00:00+08:00", "kind": "user", "text": "完整资料" * 20, "source": "fixture.jsonl:1"}]},
                "api": {"baseUrl": f"http://127.0.0.1:{server.server_port}/v1", "key": "private-key-smoke", "model": "fixture"},
                "workspace": temp,
            }
            for iteration in range(5):
                completed = subprocess.run(command, input=json.dumps(request, ensure_ascii=False), text=True, encoding="utf-8", capture_output=True, check=False)
                events = [json.loads(line) for line in completed.stdout.splitlines()]
                if iteration < 3:
                    marker = ["空正文", "截断", "过滤"][iteration]
                    assert completed.returncode == 1 and events[-1]["type"] == "error" and marker in events[-1]["message"], completed.stdout + completed.stderr
                    continue
                assert completed.returncode == 0, completed.stdout + completed.stderr
                assert events[-1] == {"type": "result", "text": report}
                if iteration == 4:
                    assert any(e.get("stage") == "恢复已完成报告" for e in events)
                assert "private-key-smoke" not in completed.stdout + completed.stderr
            assert len(calls) == 4, "Incomplete reports must retry; finished checkpoint must not call model again"
            assert calls[0]["messages"][0]["content"] == request["systemPrompt"]
            assert json.loads(calls[0]["messages"][1]["content"]) == request["input"]
            assert len(calls[1]["messages"]) == 2, "Retry must not duplicate work records"
            assert "max_tokens" not in calls[0] and "max_completion_tokens" not in calls[0]
            data = next((Path(temp) / "pipeline").glob("*/graph.sqlite")).read_bytes()
            assert b"private-key-smoke" not in data and b"PRIVATE_SYSTEM_PROMPT_SMOKE" not in data
        for model, marker in [("stream-retry-fixture", "stream closed before response.completed"), ("stream-eof-fixture", "未收到报告完成标记")]:
            with tempfile.TemporaryDirectory(prefix="report-agent-stream-") as temp:
                request["workspace"] = temp
                request["jobId"] = model
                request["api"]["model"] = model
                completed = subprocess.run(command, input=json.dumps(request, ensure_ascii=False), text=True, encoding="utf-8", capture_output=True, check=False)
                events = [json.loads(line) for line in completed.stdout.splitlines()]
                assert completed.returncode == 0, completed.stdout + completed.stderr
                assert events[-1] == {"type": "result", "text": report}
                assert any(e.get("stage") == "等待重试" and marker in e["message"] for e in events)
                assert sum(p["model"] == model for p in calls) == 2
                cached = subprocess.run(command, input=json.dumps(request, ensure_ascii=False), text=True, encoding="utf-8", capture_output=True, check=False)
                assert json.loads(cached.stdout.splitlines()[-1]) == {"type": "result", "text": report}
                assert sum(p["model"] == model for p in calls) == 2, "Only the complete result is checkpointed"
        with tempfile.TemporaryDirectory(prefix="report-agent-permanent-") as temp:
            request["workspace"] = temp
            request["jobId"] = "permanent"
            request["api"]["model"] = "permanent-fixture"
            request["input"] = {"record": "PRIVATE_WORK_RECORD"}
            completed = subprocess.run(command, input=json.dumps(request, ensure_ascii=False), text=True, encoding="utf-8", capture_output=True, check=False)
            events = [json.loads(line) for line in completed.stdout.splitlines()]
            assert completed.returncode == 1 and events[-1]["type"] == "error"
            assert "HTTP 401" in events[-1]["message"] and "Credential rejected by upstream tenant" in events[-1]["message"]
            assert "PRIVATE_" not in completed.stdout and "private-key-smoke" not in completed.stdout
            assert not any(e.get("stage") == "等待重试" for e in events)
            assert sum(p["model"] == "permanent-fixture" for p in calls) == 1
        # The connection test sends a plain string prompt, not a report payload; the
        # coverage summary must not assume a dict input (used to fail with AttributeError).
        with tempfile.TemporaryDirectory(prefix="report-agent-connection-") as temp:
            request["workspace"] = temp
            request["jobId"] = "connection-test"
            request["kind"] = "test"
            request["api"]["model"] = "connection-fixture"
            request["input"] = "测试模型连接"
            completed = subprocess.run(command, input=json.dumps(request, ensure_ascii=False), text=True, encoding="utf-8", capture_output=True, check=False)
            events = [json.loads(line) for line in completed.stdout.splitlines()]
            assert completed.returncode == 0, completed.stdout + completed.stderr
            assert events[-1]["type"] == "result" and events[-1]["text"] == report
            coverage = next(e["coverage"] for e in events if e.get("type") == "evidence")
            assert coverage["inputEvents"] == 0 and coverage["inputReports"] == 0, coverage
            assert coverage["mode"] == "direct" and not coverage["segments"], coverage
        print("PASS Agent explicit and silent stream-disconnect recovery, complete-result checkpoint, permanent error diagnostics, connection-test coverage and existing output flow")
    finally:
        server.shutdown()
        server.server_close()


if __name__ == "__main__":
    main()
