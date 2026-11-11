"""Department-local LangGraph report agent. One request on stdin, JSON events on stdout."""

from __future__ import annotations

import json
import os
from pathlib import Path
import re
import sys
from threading import Lock
import time

# The desktop application owns destinations; do not inherit ambient tracing uploads.
os.environ["LANGCHAIN_TRACING_V2"] = "false"
os.environ["LANGSMITH_TRACING"] = "false"
output_lock = Lock()


def emit(kind: str, **fields: object) -> None:
    with output_lock:
        print(json.dumps({"type": kind, **fields}, ensure_ascii=False), flush=True)


def visible_text(message: object) -> str:
    content = getattr(message, "content", "")
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        return "".join(
            block.get("text", "")
            for block in content
            if isinstance(block, dict) and block.get("type") in ("text", "output_text")
        )
    return ""


class AgentError(Exception):
    """Application-owned messages safe to show in the desktop task log."""


def incomplete_reason(message: object) -> str:
    reason = getattr(message, "response_metadata", {}).get("finish_reason")
    if not reason:
        return "模型响应连接已结束，但未收到报告完成标记；未保存不完整正文"
    if reason == "length":
        return "模型服务已截断报告正文，未保存为正式报告；请调整服务端输出能力后重试"
    if reason == "content_filter":
        return "模型服务过滤了报告输出，未保存为正式报告"
    return ""


def provider_message(error: Exception) -> str:
    body = getattr(error, "body", None)
    details = body.get("error", body) if isinstance(body, dict) else None
    if isinstance(details, dict) and isinstance(details.get("message"), str):
        return details["message"]
    if isinstance(details, str):
        return details
    return getattr(error, "message", None) or str(error)


def redact_message(message: str, request: dict | None) -> str:
    if request:
        def strings(value: object):
            if isinstance(value, str):
                yield value
            elif isinstance(value, dict):
                for nested in value.values():
                    yield from strings(nested)
            elif isinstance(value, list):
                for nested in value:
                    yield from strings(nested)

        sensitive = [request.get("api", {}).get("key", ""), request.get("systemPrompt", "")]
        sensitive.extend(strings(request.get("input", {})))
        for value in sorted(set(sensitive), key=len, reverse=True):
            if not value:
                continue
            fragments = [value, *(line for line in value.splitlines() if len(line) >= 8)]
            for fragment in fragments:
                for form in (fragment, json.dumps(fragment, ensure_ascii=False)[1:-1], json.dumps(fragment, ensure_ascii=True)[1:-1]):
                    message = message.replace(form, "[已脱敏]")
    message = re.sub(r"(?i)\b(?:Bearer\s+|sk-)[a-z0-9._~+/=-]+", "[认证信息已脱敏]", message)
    message = re.sub(r"(?i)\b(?:api[_-]?key|authorization|system[_-]?prompt|prompt|request[_-]?body|input|content)\s*[:=]\s*[^\r\n]+", "[请求内容已脱敏]", message)
    return message


def error_message(error: Exception, request: dict | None = None) -> str:
    if isinstance(error, AgentError):
        return str(error)
    status = getattr(error, "status_code", None)
    body = getattr(error, "body", None)
    details = body.get("error", body) if isinstance(body, dict) else {}
    code = details.get("code") if isinstance(details, dict) else None
    known = {
        "context_length_exceeded": "模型服务无法容纳本次完整资料，请使用支持更长上下文的模型",
        "model_not_found": "模型不存在或当前凭据无权使用",
        "invalid_api_key": "API Key 无效",
        "insufficient_quota": "模型服务提示可用额度不足",
        "unsupported_parameter": "模型服务不支持请求中的参数",
        "unsupported_value": "模型服务不支持请求中的参数取值",
    }
    if status:
        description = known.get(code) if isinstance(code, str) else None
        if not description:
            description = {
                400: "模型服务拒绝请求，请按任务时间核对服务端错误记录",
                401: "身份认证失败，请检查 API Key",
                403: "模型服务拒绝访问，请检查权限或网关策略",
                404: "接口或模型不存在，请检查地址和模型名称",
                413: "模型服务拒绝了过大的请求体",
                429: "模型服务当前限流或额度不足",
            }.get(status, "模型服务请求失败，请按任务时间核对服务端记录")
        detail = redact_message(provider_message(error), request)
        return f"模型服务 HTTP {status}；{description}" + (f"；服务端原因：{detail}" if detail else "")
    if isinstance(error, json.JSONDecodeError):
        return "Agent 调用数据不是有效 JSON"
    if isinstance(error, (KeyError, TypeError)):
        return "Agent 调用数据缺少必要字段或字段格式不正确"
    if isinstance(error, ModuleNotFoundError):
        return "Agent 运行时依赖缺失，请重新安装组长端"
    if type(error).__name__ in ("APIConnectionError", "ConnectError"):
        return "模型连接中断；" + redact_message(provider_message(error), request)
    return f"Agent 执行失败（{type(error).__name__}）；" + redact_message(provider_message(error), request)


def transient_model_error(error: Exception) -> bool:
    from openai import APIConnectionError, APIError
    import httpx
    import httpx2

    if isinstance(error, (APIConnectionError, ConnectionError, TimeoutError, httpx.NetworkError, httpx.RemoteProtocolError, httpx.TimeoutException, httpx2.NetworkError, httpx2.RemoteProtocolError, httpx2.TimeoutException)):
        return True
    if isinstance(error, APIError) and not getattr(error, "status_code", None):
        message = provider_message(error).lower()
        return any(marker in message for marker in ("stream disconnected", "stream closed before", "connection reset", "connection closed", "connection aborted", "incomplete chunked read", "peer closed connection"))
    return False


def generate(request: dict) -> str:
    from report_pipeline import ReportPipeline
    return ReportPipeline(request, emit, error_message, transient_model_error).run()


def main() -> int:
    for stream in (sys.stdin, sys.stdout, sys.stderr):
        if hasattr(stream, "reconfigure"):
            stream.reconfigure(encoding="utf-8")
    request: dict = {}
    try:
        request = json.loads(sys.stdin.readline())
        emit("result", text=generate(request))
        return 0
    except Exception as error:
        emit("error", message=error_message(error, request))
        return 1


if __name__ == "__main__":
    # Pipeline helpers import this module; keep one AgentError identity in script/frozen runs.
    sys.modules["runner"] = sys.modules[__name__]
    raise SystemExit(main())
