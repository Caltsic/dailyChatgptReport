"""Token-aware, resumable report workflow. Raw records stay in the submitted snapshot."""
from __future__ import annotations

from collections import OrderedDict
from concurrent.futures import ThreadPoolExecutor
from contextlib import contextmanager
from datetime import datetime, timezone
import hashlib
import json
import operator
import os
from pathlib import Path
import sys
import threading
import time
from typing import Annotated, TypedDict
import uuid

PIPELINE_VERSION = "evidence.v1"


def encoded(value: object) -> str:
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"), sort_keys=True)


def digest(value: object) -> str:
    return hashlib.sha256(encoded(value).encode("utf-8")).hexdigest()


def write_json(path: Path, value: object) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(f".{uuid.uuid4().hex}.tmp")
    temporary.write_text(encoded(value), encoding="utf-8")
    temporary.replace(path)


class ContextOverflow(Exception):
    pass


class EvidenceError(Exception):
    pass


def context_overflow(error: Exception) -> bool:
    body = getattr(error, "body", None)
    detail = body.get("error", body) if isinstance(body, dict) else {}
    code = detail.get("code", "") if isinstance(detail, dict) else ""
    message = str(detail.get("message", error) if isinstance(detail, dict) else error).lower()
    return code in ("context_length_exceeded", "context_window_exceeded") or (
        getattr(error, "status_code", None) in (400, 413) and any(term in message for term in (
            "exceeds the context window", "maximum context length", "context length exceeded",
            "exceed context window", "too many tokens", "input is too long",
        ))
    )


class Slots:
    """One reader routes host grants to concurrent LangGraph worker threads."""
    def __init__(self, hosted: bool, emit):
        self.hosted, self.emit = hosted, emit
        self.lock = threading.Lock()
        self.waiting: dict[str, threading.Event] = {}
        self.closed = False
        self.local = threading.Semaphore(4)
        if hosted:
            threading.Thread(target=self._read, daemon=True).start()

    def _read(self) -> None:
        try:
            for line in sys.stdin:
                grant = json.loads(line)
                if grant.get("type") == "slot":
                    with self.lock:
                        event = self.waiting.get(grant.get("requestId"))
                        if event:
                            event.set()
        finally:
            with self.lock:
                self.closed = True
                for event in self.waiting.values():
                    event.set()

    @contextmanager
    def acquire(self):
        if not self.hosted:
            with self.local:
                yield
            return
        request_id = uuid.uuid4().hex
        event = threading.Event()
        with self.lock:
            if self.closed:
                raise RuntimeError("组长端请求调度连接已关闭")
            self.waiting[request_id] = event
        self.emit("request_slot", requestId=request_id)
        try:
            event.wait()
            if self.closed:
                raise RuntimeError("组长端请求调度连接已关闭")
            yield
        finally:
            with self.lock:
                self.waiting.pop(request_id, None)
            self.emit("release_slot", requestId=request_id)


class GraphState(TypedDict, total=False):
    mode: str
    chunks: list[str]
    packets: Annotated[list[dict], operator.add]
    facts: list[dict]
    result: str


class ReportPipeline:
    def __init__(self, request: dict, emit, error_message, transient_error):
        import tiktoken
        self.request, self.emit = request, emit
        self.error_message, self.transient_error = error_message, transient_error
        self.api = request["api"]
        self.input = request["input"]
        self.kind = request.get("kind", "personal")
        self.prompts = request.get("prompts", {})
        self.prompt = request["systemPrompt"]
        bundled = Path(getattr(sys, "_MEIPASS", "")) / "tokenizer-cache"
        if getattr(sys, "frozen", False) and bundled.is_dir():
            os.environ["TIKTOKEN_CACHE_DIR"] = str(bundled)
        try:
            self.encoding = tiktoken.encoding_for_model(self.api["model"])
        except KeyError:
            self.encoding = tiktoken.get_encoding("o200k_base")
        self.window = int(self.api.get("contextTokens") or 65536)
        self.reserve = int(self.api.get("outputReserveTokens") or 8192)
        self.limit = self.window - self.reserve
        if self.limit < 1024:
            raise ValueError("上下文容量应大于输出预留，并留出至少 1024 Token 输入空间")
        self.target = min(int(self.api.get("chunkTokens") or 48000), self.limit)
        namespace = digest({"version": PIPELINE_VERSION, "input": self.input, "endpoint": self.api["baseUrl"].rstrip("/"),
            "model": self.api["model"], "prompt": self.prompt, "prompts": self.prompts,
            "window": self.window, "reserve": self.reserve, "target": self.target, "encoding": self.encoding.name})
        self.root = Path(request["workspace"]) / "pipeline" / namespace
        self.root.mkdir(parents=True, exist_ok=True)
        self.lock = threading.RLock()
        self.slots = Slots(bool(request.get("slotProtocol")), emit)
        self.chunks: dict[str, dict] = {}
        self.sources: dict[str, dict] = {}
        self.originals: dict[str, dict] = {}
        self.pieces: dict[str, dict] = {}
        manifest = self.root / "manifest.json"
        if manifest.exists():
            self.pieces = json.loads(manifest.read_text(encoding="utf-8")).get("pieces", {})
            for item in self.pieces.values():
                if item["state"] in ("running", "waiting", "retrying", "failed"):
                    item["state"] = "pending"
                    item.pop("error", None)
        self.requests = self.retries = 0
        self.last_progress = 0.0
        self.estimated = self.request_tokens(self.prompt, self.input)

    def tokens(self, text: str) -> int:
        return len(self.encoding.encode(text, disallowed_special=()))

    def request_tokens(self, prompt: str, value: object) -> int:
        # Tokenizer estimates include serialized messages; the endpoint's actual window wins.
        return self.tokens(encoded([{"role": "system", "content": prompt}, {"role": "user", "content": encoded(value)}])) + 16

    def emit_progress(self, stage: str, message: str, force: bool = True) -> None:
        with self.lock:
            now = time.monotonic()
            if not force and now - self.last_progress < 1:
                return
            self.last_progress = now
            pieces = [{key: value[key] for key in ("id", "label", "state", "error") if key in value}
                for value in self.pieces.values() if value["state"] != "split"]
            self.emit("progress", stage=stage, message=message,
                completed=sum(v["state"] == "completed" for v in pieces), total=len(pieces),
                active=sum(v["state"] == "running" for v in pieces), failed=sum(v["state"] == "failed" for v in pieces),
                pieces=pieces, estimatedTokens=self.estimated, requests=self.requests, retries=self.retries)

    def update(self, key: str, label: str, state: str, stage: str, message: str = "", **fields) -> None:
        with self.lock:
            item = self.pieces.setdefault(key, {"id": key, "label": label})
            item.update(state=state, **fields)
            if state != "failed":
                item.pop("error", None)
            write_json(self.root / "manifest.json", {"version": PIPELINE_VERSION, "pieces": self.pieces})
            self.emit_progress(stage, message or f"{label} · {state}")

    def safe_error(self, error: Exception, prompt: str, payload: object) -> str:
        return self.error_message(error, {**self.request, "systemPrompt": prompt, "input": payload})

    def call(self, prompt: str, payload: object, key: str, label: str, stage: str) -> str:
        from langchain.agents import create_agent
        from langchain.agents.middleware import ModelRetryMiddleware, wrap_model_call
        from langchain_core.messages import AIMessage, HumanMessage
        from langchain_openai import ChatOpenAI
        from runner import AgentError, incomplete_reason, visible_text
        if self.request_tokens(prompt, payload) > self.limit:
            raise ContextOverflow("估算请求已超过工作上下文容量")
        model = ChatOpenAI(model=self.api["model"], base_url=self.api["baseUrl"].rstrip("/"),
            api_key=self.api["key"], timeout=None, streaming=True, stream_usage=False, max_retries=0)
        attempts = 0

        def retry_on(error: Exception) -> bool:
            body = getattr(error, "body", {})
            details = body.get("error", body) if isinstance(body, dict) else {}
            rate = getattr(error, "status_code", None) == 429 and (not isinstance(details, dict) or details.get("code") != "insufficient_quota")
            transient = self.transient_error(error) or rate or getattr(error, "status_code", 0) in (500, 502, 503, 504)
            if not transient or attempts >= (6 if rate else 3):
                return False
            with self.lock:
                self.retries += 1
            delay = min(60, (10 if rate else 1) * 2 ** (attempts - 1))
            response = getattr(error, "response", None)
            retry_after = response.headers.get("retry-after") if response is not None else None
            if retry_after:
                try:
                    delay = min(60, max(delay, float(retry_after)))
                except ValueError:
                    pass
            self.update(key, label, "retrying", "等待重试",
                f"{label}：{self.safe_error(error, prompt, payload)}；{delay:g} 秒后重试")
            time.sleep(delay)
            return True

        @wrap_model_call
        def slot_and_completion(model_request, handler):
            nonlocal attempts
            self.update(key, label, "waiting", stage, f"{label}：等待模型请求名额")
            with self.slots.acquire():
                with self.lock:
                    self.requests += 1
                    attempts += 1
                self.update(key, label, "running", stage, f"{label}：模型正在处理")
                response = handler(model_request)
                for message in response.result:
                    if isinstance(message, AIMessage):
                        reason = incomplete_reason(message)
                        if reason:
                            if not message.response_metadata.get("finish_reason"):
                                raise ConnectionError(reason)
                            raise AgentError(reason)
                return response

        # Each attempt is a fresh request. Only validated evidence is checkpointed by this pipeline.
        agent = create_agent(model=model, tools=[], system_prompt=prompt, checkpointer=False,
            middleware=[ModelRetryMiddleware(max_retries=5, retry_on=retry_on, on_failure="error", initial_delay=0, jitter=False), slot_and_completion])
        final = None
        received = 0
        try:
            for mode, event in agent.stream({"messages": [HumanMessage(content=encoded(payload))]}, stream_mode=["messages", "updates"]):
                if mode == "messages":
                    message, _ = event
                    received += len(visible_text(message))
                    self.emit_progress(stage, f"{label}：已收到 {received} 字符", force=False)
                elif mode == "updates":
                    for update in event.values():
                        if isinstance(update, dict) and update.get("messages"):
                            for message in update["messages"]:
                                if isinstance(message, AIMessage):
                                    final = message
        except Exception as error:
            if context_overflow(error):
                raise ContextOverflow(self.safe_error(error, prompt, payload)) from error
            raise
        if final is None or not visible_text(final).strip():
            raise AgentError("模型返回了空正文，请重试当前分片")
        if problem := incomplete_reason(final):
            raise AgentError(problem)
        return visible_text(final).strip()

    def parse_facts(self, text: str, allowed: set[str]) -> dict:
        stripped = text.strip()
        if stripped.startswith("```"):
            stripped = stripped.split("\n", 1)[-1].rsplit("```", 1)[0].strip()
        try:
            answer = json.loads(stripped)
        except json.JSONDecodeError as error:
            raise EvidenceError("证据结果不是 JSON 对象") from error
        if not isinstance(answer, dict) or not isinstance(answer.get("facts"), list):
            raise EvidenceError("证据结果缺少 facts 列表")
        for fact in answer["facts"]:
            if not isinstance(fact, dict) or not isinstance(fact.get("text"), str) or not fact["text"].strip():
                raise EvidenceError("证据项缺少事实说明")
            refs = fact.get("refs")
            if not isinstance(refs, list) or not refs or not all(isinstance(ref, str) and ref in allowed for ref in refs):
                raise EvidenceError("证据引用不属于本次输入")
            fact["refs"] = list(dict.fromkeys(refs))
        needed = answer.get("needsContext", [])
        if not isinstance(needed, list) or not all(isinstance(ref, str) and ref in allowed for ref in needed):
            raise EvidenceError("原文回查引用不属于本次输入")
        answer["needsContext"] = list(dict.fromkeys(needed))
        return answer

    def evidence_call(self, prompt: str, payload: dict, allowed: set[str], key: str, label: str, stage: str) -> dict:
        path = self.root / "results" / f"{key}.json"
        if path.exists():
            answer = self.parse_facts(path.read_text(encoding="utf-8"), allowed)
            self.update(key, label, "completed", stage, f"{label}：复用已完成证据")
            return answer
        try:
            for attempt in range(2):
                material = payload if not attempt else {**payload, "formatCorrection": "上一次结果格式或引用无效。请仅返回 facts JSON；refs 必须来自本次输入。"}
                text = self.call(prompt, material, key, label, stage)
                try:
                    answer = self.parse_facts(text, allowed)
                    break
                except EvidenceError:
                    if attempt:
                        raise
                    self.emit_progress(stage, f"{label}：修正当前分片的证据格式或引用")
            write_json(path, answer)
            self.update(key, label, "completed", stage)
            return answer
        except ContextOverflow:
            self.update(key, label, "split", stage, f"{label}：容量不足，细分当前材料")
            raise
        except Exception as error:
            message = self.safe_error(error, prompt, payload)
            self.update(key, label, "failed", stage, f"{label}：{message}", error=message)
            raise

    def metadata(self) -> dict:
        return {key: value for key, value in self.input.items() if key not in ("events", "reports")}

    def fragment(self, event: dict, start: int, end: int, context_only: bool = False) -> dict:
        item = {key: value for key, value in event.items() if key != "text"}
        item.update(ref=f"{event['eventId']}@{start}:{end}", start=start, end=end, text=event["text"][start:end])
        if context_only:
            item["contextOnly"] = True
        self.sources[item["ref"]] = item
        return item

    def raw_groups(self) -> OrderedDict[str, list[dict]]:
        groups: OrderedDict[str, list[dict]] = OrderedDict()
        if self.kind == "department":
            for report in self.input.get("reports", []):
                rid = str(report["id"])
                common = {"sessionId": rid, "reportId": rid, "author": report.get("memberName", ""), "version": report.get("version"), "timestamp": report.get("createdAt", "")}
                rows = [{**common, "eventId": f"{rid}/body", "kind": "report", "source": f"{common['author']} 日报 V{common['version']}", "text": report.get("body", "")}]
                supplement = report.get("supplement") or {}
                rows.append({**common, "eventId": f"{rid}/supplement", "kind": "supplement", "personalStatement": True,
                    "author": supplement.get("author", common["author"]), "timestamp": supplement.get("submittedAt", ""),
                    "source": f"{common['author']} 个人补充 V{common['version']}", "text": supplement.get("text", "")})
                groups[rid] = rows
        else:
            for ordinal, event in enumerate(self.input.get("events", [])):
                sid = str(event.get("sessionId", "unknown"))
                groups.setdefault(sid, []).append({**event, "eventId": str(event.get("id") or f"event-{ordinal}")})
            for rows in groups.values():
                def occurred(row):
                    value = row.get("timestamp", "")
                    parsed = datetime.fromisoformat(value.replace("Z", "+00:00")) if value else datetime.min.replace(tzinfo=timezone.utc)
                    return parsed.replace(tzinfo=timezone.utc) if parsed.tzinfo is None else parsed
                rows.sort(key=occurred)
        self.originals = {row["eventId"]: row for rows in groups.values() for row in rows}
        return groups

    def split_event(self, event: dict, allowance: int) -> list[dict]:
        text = event.get("text", "")
        if not text:
            return [self.fragment(event, 0, 0)]
        if self.tokens(text) <= allowance:
            return [self.fragment(event, 0, len(text))]
        spans, start = [], 0
        while start < len(text):
            lo, hi = start + 1, min(len(text), start + allowance * 4)
            while lo < hi:
                mid = (lo + hi + 1) // 2
                if self.tokens(text[start:mid]) <= allowance:
                    lo = mid
                else:
                    hi = mid - 1
            spans.append(self.fragment(event, start, lo))
            start = lo
        return spans

    def extraction_payload(self, chunk: dict) -> dict:
        return {"kind": self.kind, "context": self.metadata(), "scope": chunk["session"], "segments": chunk["segments"]}

    def register_chunk(self, session: str, segments: list[dict], label: str) -> str:
        key = "extract-" + digest({"session": session, "segments": segments})[:24]
        chunk = {"id": key, "session": session, "segments": segments, "label": label}
        self.chunks[key] = chunk
        with self.lock:
            self.pieces.setdefault(key, {"id": key, "label": label, "state": "pending", "stage": "extract",
                "session": session,
                "spans": [{"eventId": item["eventId"], "start": item["start"], "end": item["end"], "contextOnly": bool(item.get("contextOnly"))} for item in segments],
                "ranges": [{"ref": item["ref"], "eventId": item["eventId"], "start": item["start"], "end": item["end"]} for item in segments if not item.get("contextOnly")]})
        return key

    def boundary_context(self, item: dict, tail: bool = False) -> dict:
        start, end = item["start"], item["end"]
        if tail:
            start = max(start, end - 400)
        else:
            end = min(end, start + 400)
        return self.fragment(self.originals[item["eventId"]], start, end, True)

    def plan(self) -> list[str]:
        if not self.prompts.get("extract") or not self.prompts.get("merge"):
            raise ValueError("缺少分段分析提示词，请更新完整组长端安装包")
        overhead = self.request_tokens(self.prompts["extract"], {"kind": self.kind, "context": self.metadata(), "segments": []})
        allowance = min(self.target, self.limit) - overhead - 2048
        if allowance < 512:
            raise ValueError("分段输入容量不足，请增加模型上下文容量或减少输出预留")
        planned = []
        for group_index, (session, rows) in enumerate(self.raw_groups().items(), 1):
            spans = [span for row in rows for span in self.split_event(row, allowance)]
            packets, current, size = [], [], 0
            for span in spans:
                cost = self.tokens(encoded(span)) + 8
                if current and size + cost > allowance:
                    packets.append(current)
                    current, size = [], 0
                current.append(span)
                size += cost
            if current:
                packets.append(current)
            goal = None
            for row in rows:
                if row.get("kind") not in ("user", "user_message"):
                    continue
                text, start = row.get("text", ""), 0
                if text.lstrip().startswith("Side conversation boundary"):
                    continue
                if text.lstrip().startswith("<environment_context>") and "</environment_context>" in text:
                    start = text.index("</environment_context>") + len("</environment_context>")
                if text[start:].strip():
                    start += len(text[start:]) - len(text[start:].lstrip())
                    goal = self.fragment(row, start, min(len(text), start+600), True)
                    break
            for index, packet in enumerate(packets):
                segments = list(packet)
                if index:
                    prior = packets[index - 1][-1]
                    segments.insert(0, self.boundary_context(prior, tail=True))
                if index + 1 < len(packets):
                    segments.append(self.boundary_context(packets[index + 1][0]))
                if goal and not any(item["eventId"] == goal["eventId"] for item in segments):
                    segments.insert(0, goal)
                planned.append(self.register_chunk(session, segments, f"{'员工报告' if self.kind == 'department' else '会话'} {group_index} · 分段 {index+1}/{len(packets)}"))
        # Adaptive child spans are persisted as offsets, so resumed graph packets keep usable refs.
        for key, item in list(self.pieces.items()):
            if item.get("stage") == "extract" and key not in self.chunks and item.get("spans"):
                spans = [self.fragment(self.originals[span["eventId"]], span["start"], span["end"], span["contextOnly"]) for span in item["spans"]]
                self.chunks[key] = {"id": key, "session": item["session"], "label": item["label"], "segments": spans}
        write_json(self.root / "plan.json", {"estimatedTokens": self.estimated, "tokenizer": self.encoding.name,
            "capacity": self.window, "outputReserve": self.reserve, "chunkIds": planned,
            "chunks": [{key: value for key, value in self.chunks[cid].items() if key != "segments"} for cid in planned]})
        self.emit_progress("分段规划", f"估算输入 {self.estimated:,} Token，分为 {len(planned)} 段；工具关联按原事件顺序保留")
        return planned

    def extract(self, chunk_id: str) -> dict:
        chunk = self.chunks[chunk_id]
        existing = self.pieces.get(chunk_id, {})
        if existing.get("state") == "split" and existing.get("children"):
            facts = [fact for child in existing["children"] for fact in self.extract(child)["facts"]]
            return {"id": chunk_id, "session": chunk["session"], "facts": facts}
        segments = chunk["segments"]
        allowed = {item["ref"] for item in segments}
        try:
            answer = self.evidence_call(self.prompts["extract"], self.extraction_payload(chunk), allowed,
                chunk_id, chunk["label"], "分段分析")
            return {"id": chunk_id, "session": chunk["session"], "facts": answer["facts"]}
        except ContextOverflow:
            primary = [item for item in segments if not item.get("contextOnly")]
            if len(primary) > 1:
                pivot = len(primary) // 2
                partitions = [primary[:pivot], primary[pivot:]]
            else:
                span = primary[0]
                text = span["text"]
                if len(text) < 2:
                    raise ValueError("模型上下文无法容纳最小记录片段，请核对服务端容量")
                pivot = len(text) // 2
                partitions = [[{**span, "end": span["start"]+pivot, "text": text[:pivot], "ref": f"{span['eventId']}@{span['start']}:{span['start']+pivot}"}],
                    [{**span, "start": span["start"]+pivot, "text": text[pivot:], "ref": f"{span['eventId']}@{span['start']+pivot}:{span['end']}"}]]
            children = []
            for index, partition in enumerate(partitions, 1):
                for item in partition:
                    self.sources[item["ref"]] = item
                context = [item for item in segments if item.get("contextOnly")]
                adjacent = partitions[1][0] if index == 1 else partitions[0][-1]
                context.append(self.boundary_context(adjacent, tail=index == 2))
                child = self.register_chunk(chunk["session"], context + partition, f"{chunk['label']} · 细分 {index}")
                children.append(child)
            self.update(chunk_id, chunk["label"], "split", "分段分析", f"{chunk['label']}：细分为 {len(children)} 段", children=children)
            facts = [fact for child in children for fact in self.extract(child)["facts"]]
            return {"id": chunk_id, "session": chunk["session"], "facts": facts}

    def deduplicate(self, facts: list[dict]) -> list[dict]:
        seen, output = set(), []
        for fact in facts:
            key = digest(fact)
            if key not in seen:
                seen.add(key)
                output.append(fact)
        return output

    def review_context(self, facts: list[dict], needed: list[str], scope: str) -> list[dict]:
        """Read requested raw ranges locally; oversized ranges are divisible, including one fact."""
        reviewed = [fact for fact in facts if not set(fact["refs"]).intersection(needed)]

        def read(values: list[dict], source: dict) -> list[dict]:
            payload = {"kind": self.kind, "scope": scope, "facts": values, "sourceContext": [source],
                "contextReview": "这是所请求原文的一部分，readRange 表示本次读取位置。refs 沿用原引用；保留本片段未能解决的差异，不把局部未见当作不存在。其他片段将在之后统一核对。"}
            allowed = {source["ref"], *(ref for fact in values for ref in fact["refs"])}
            key = "context-"+digest(payload)[:24]
            try:
                return self.evidence_call(self.prompts["merge"], payload, allowed, key, f"原文回查 · {scope}", "原文核对")["facts"]
            except ContextOverflow:
                text = source["text"]
                if len(text) < 2:
                    raise
                # Facts themselves must fit before splitting raw text, avoiding futile tiny reads.
                if self.request_tokens(self.prompts["merge"], {**payload, "sourceContext": []}) >= self.limit:
                    raise
                middle = len(text)//2
                start, end = source.get("readRange", [source["start"], source["end"]])
                left = {**source, "text": text[:middle], "readRange": [start, start+middle]}
                right = {**source, "text": text[middle:], "readRange": [start+middle, end]}
                return read(values, left) + read(values, right)

        for ref in needed:
            values = [fact for fact in facts if ref in fact["refs"]]
            reviewed.extend(read(values, self.sources[ref]))
        return self.merge(reviewed, scope+"/原文核对完成", allow_context=False)

    def merge(self, facts: list[dict], scope: str, allow_context: bool = True) -> list[dict]:
        facts = self.deduplicate(facts)
        if not facts:
            return []
        payload = {"kind": self.kind, "scope": scope, "facts": facts}
        if not allow_context:
            payload["contextReview"] = "所请求的原文已分批完整核对。现在统一归并；仍不确定的差异保留在对应事实旁。"
        key = "merge-" + digest(payload)[:24]
        label = f"核对归并 · {scope}"
        allowed = {ref for fact in facts for ref in fact["refs"]}
        try:
            answer = self.evidence_call(self.prompts["merge"], payload, allowed, key, label, "核对归并")
            if allow_context and answer.get("needsContext"):
                needed = answer["needsContext"]
                # A context request is provisional, including when no facts are returned yet.
                review_facts = self.deduplicate(answer["facts"] + facts)
                return self.review_context(review_facts, needed, scope)
            return answer["facts"]
        except ContextOverflow:
            if len(facts) < 2:
                raise ValueError("单项证据及所需原文仍超过模型容量，请增加上下文容量后重试；未生成残缺日报")
            pivot = len(facts)//2
            smaller = self.merge(facts[:pivot], scope+"/前段", allow_context) + self.merge(facts[pivot:], scope+"/后段", allow_context)
            if self.tokens(encoded(smaller)) >= self.tokens(encoded(facts)):
                raise ValueError("证据归并后仍超过模型容量，未能进一步压缩；已保留完成分片，可调整上下文容量后重试")
            return self.merge(smaller, scope+"/合并", allow_context)

    def final_payload(self, facts: list[dict]) -> dict:
        return {**self.metadata(), "evidence": self.timed_facts(facts), "analysisScope": "全部提交片段已完成分析。recordedTimeRange 是来源事件的记录时间，供跨会话核对先后，不是工时；同一对象的后续修正与早期意见结合判断。refs 为内部来源定位，成稿按读者需要组织，不输出引用目录。"}

    def timed_facts(self, facts: list[dict]) -> list[dict]:
        result = []
        for fact in facts:
            stamps = []
            for ref in fact["refs"]:
                value = self.sources.get(ref, {}).get("timestamp")
                if value:
                    stamp = datetime.fromisoformat(value.replace("Z", "+00:00"))
                    stamps.append(stamp.replace(tzinfo=timezone.utc) if stamp.tzinfo is None else stamp.astimezone(timezone.utc))
            result.append({**fact, **({"recordedTimeRange": [min(stamps).isoformat(), max(stamps).isoformat()]} if stamps else {})})
        return result

    def reconcile(self, original: list[dict], baseline: list[dict]) -> list[dict]:
        original = self.deduplicate(original)
        # When extracted evidence already fits, give the writer all of it without lossy reduction.
        if self.request_tokens(self.prompt, self.final_payload(original)) <= self.limit:
            return original
        baseline = self.timed_facts(baseline)
        context = {"kind": self.kind, "scope": "跨会话执行状态核对",
            "statusReview": "facts 是未经归并的原始抽取证据，baselineFacts 是压缩后的候选事实。只补回候选事实遗漏或误表述的实际执行、修正、失败、验证范围及当前待办，返回有原始 refs 的 facts；没有需要补充的内容可返回空列表。特别核对早期故障后来已修正、讨论后来已实施、曾完成后来又失败的情况。recordedTimeRange 是日志记录时间，不是工时。", "contextReview": "本次对照原始抽取证据核对压缩遗漏；最终会连同候选事实一起成稿。"}
        original = self.timed_facts(original)
        largest = max(original, key=lambda fact: self.tokens(encoded(fact)), default={})
        while self.request_tokens(self.prompts["merge"], {**context, "baselineFacts": baseline, "facts": [largest]}) > min(self.target, self.limit):
            smaller = self.timed_facts(self.merge(baseline, "跨会话核对候选事实容量归并"))
            if self.tokens(encoded(smaller)) >= self.tokens(encoded(baseline)):
                raise ValueError("跨会话候选事实归并后仍无法容纳核对材料；完成证据已保留")
            baseline = smaller
        context["baselineFacts"] = baseline
        batches, batch = [], []
        for fact in original:
            candidate = batch + [fact]
            if batch and self.request_tokens(self.prompts["merge"], {**context, "facts": candidate}) > min(self.target, self.limit):
                batches.append(batch)
                batch = []
            batch.append(fact)
        if batch:
            batches.append(batch)

        def check(entry):
            index, values = entry
            payload = {**context, "facts": values}
            allowed = {ref for fact in baseline + values for ref in fact["refs"]}
            label = f"跨会话状态核对 {index}"
            try:
                answer = self.evidence_call(self.prompts["merge"], payload, allowed,
                    "reconcile-"+digest(payload)[:24], label, "跨会话核对")
                if answer.get("needsContext"):
                    return self.review_context(self.deduplicate(values + answer["facts"]), answer["needsContext"], label)
                return answer["facts"]
            except ContextOverflow:
                if len(values) < 2:
                    raise ValueError("状态核对的单项证据和候选事实仍超出接口容量；已保存完成分片")
                middle = len(values)//2
                return check((f"{index}.1", values[:middle])) + check((f"{index}.2", values[middle:]))

        with ThreadPoolExecutor(max_workers=4) as executor:
            additions = [fact for values in executor.map(check, enumerate(batches, 1)) for fact in values]
        return self.deduplicate(baseline + additions)

    def write_report(self, facts: list[dict]) -> str:
        from runner import AgentError
        facts = self.deduplicate(facts)
        while True:
            payload = self.final_payload(facts)
            key = "write-"+digest(payload)[:24]
            try:
                text = self.call(self.prompt, payload, key, "统一撰写日报" if self.kind != "department" else "统一撰写部门汇总", "撰写报告")
                self.update(key, "最终报告", "completed", "报告已生成")
                return text
            except ContextOverflow:
                self.update(key, "最终报告", "split", "核对归并", "成稿输入超过容量，继续归并已完成证据")
                before = self.tokens(encoded(facts))
                facts = self.merge(facts, "最终成稿证据")
                if self.tokens(encoded(facts)) >= before:
                    raise AgentError("归并证据仍不能容纳于模型上下文，未生成不完整报告；请核对模型容量后重试")
            except Exception as error:
                message = self.safe_error(error, self.prompt, payload)
                self.update(key, "最终报告", "failed", "成稿失败", message, error=message)
                raise

    def coverage(self) -> dict:
        primary = [p for p in self.pieces.values() if p.get("stage") == "extract" and p["state"] != "split"]
        # The connection test sends a plain prompt string instead of a report payload, so
        # the summary must not assume a dict input.
        payload = self.input if isinstance(self.input, dict) else {}

        def count(key: str) -> int:
            value = payload.get(key)
            return len(value) if isinstance(value, list) else 0

        return {"pipelineVersion": PIPELINE_VERSION, "mode": "staged" if primary else "direct", "estimatedTokens": self.estimated, "tokenizer": self.encoding.name,
            "inputEvents": count("events"), "inputReports": count("reports"),
            "segments": [{key: item[key] for key in ("id", "label", "state", "ranges") if key in item} for item in primary],
            "complete": all(item["state"] == "completed" for item in primary)}

    def finish(self, result: str) -> str:
        sources = {ref: f"{item.get('source', item.get('eventId',''))} · {item.get('timestamp','')} · 字符 {item['start']}–{item['end']}" for ref, item in self.sources.items()}
        evidence = {"sources": sources, "coverage": self.coverage()}
        write_json(self.root / "evidence.json", evidence)
        write_json(self.root / "final.json", {"text": result})
        self.emit("evidence", **evidence)
        return result

    def run(self) -> str:
        from langgraph.graph import StateGraph, START, END
        from langgraph.types import Send
        from langgraph.checkpoint.sqlite import SqliteSaver
        cached = self.root / "final.json"
        if cached.exists():
            self.emit_progress("恢复已完成报告", "已读取此任务保存的完整报告")
            evidence = self.root / "evidence.json"
            if evidence.exists():
                self.emit("evidence", **json.loads(evidence.read_text(encoding="utf-8")))
            return json.loads(cached.read_text(encoding="utf-8"))["text"]
        # Rebuild deterministic spans in memory on every start; graph checkpoints contain IDs/facts only.
        planned = self.plan() if self.estimated > self.limit and self.kind != "test" else []
        graph = StateGraph(GraphState)

        def prepare(state):
            return {"mode": "staged" if planned else "direct", "chunks": planned}

        def direct(state):
            key = "direct"
            try:
                result = self.call(self.prompt, self.input, key, "完整资料成稿", "生成报告")
                self.update(key, "完整资料成稿", "completed", "报告已生成")
                return {"result": result}
            except ContextOverflow:
                self.update(key, "完整资料成稿", "split", "分段规划", "服务端提示上下文不足，改为分段分析")
                return {"mode": "staged", "chunks": self.plan()}
            except Exception as error:
                message = self.safe_error(error, self.prompt, self.input)
                self.update(key, "完整资料成稿", "failed", "生成失败", message, error=message)
                raise

        def route(state):
            if state.get("result"):
                return END
            if state["mode"] == "direct":
                return "direct"
            return [Send("extract", {"chunkId": cid}) for cid in state["chunks"]] or "aggregate"

        def extract(state):
            return {"packets": [self.extract(state["chunkId"])]}

        def aggregate(state):
            grouped: OrderedDict[str, list[dict]] = OrderedDict()
            order = {cid: index for index, cid in enumerate(state["chunks"])}
            packets = sorted(state.get("packets", []), key=lambda packet: order[packet["id"]])
            for packet in packets:
                grouped.setdefault(packet["session"], []).append(packet)
            def consolidate(entry):
                index, packets = entry
                values = [fact for packet in packets for fact in packet["facts"]]
                return self.merge(values, f"{'员工报告' if self.kind == 'department' else '会话'} {index}") if len(packets)>1 else values
            with ThreadPoolExecutor(max_workers=4) as executor:
                facts = [fact for values in executor.map(consolidate, enumerate(grouped.values(), 1)) for fact in values]
            return {"facts": self.deduplicate(facts)}

        graph.add_node("prepare", prepare)
        graph.add_node("direct", direct)
        graph.add_node("extract", extract)
        graph.add_node("aggregate", aggregate)
        graph.add_node("reconcile", lambda state: {"facts": self.reconcile(
            [fact for packet in state["packets"] for fact in packet["facts"]], state["facts"])})
        graph.add_node("write", lambda state: {"result": self.write_report(state["facts"])})
        graph.add_edge(START, "prepare")
        graph.add_conditional_edges("prepare", route)
        graph.add_conditional_edges("direct", route)
        graph.add_edge("extract", "aggregate")
        graph.add_edge("aggregate", "reconcile")
        graph.add_edge("reconcile", "write")
        graph.add_edge("write", END)
        with SqliteSaver.from_conn_string(str(self.root / "graph.sqlite")) as saver:
            workflow = graph.compile(checkpointer=saver)
            config = {"configurable": {"thread_id": self.request["jobId"]}, "max_concurrency": 4}
            snapshot = workflow.get_state(config)
            if snapshot.values.get("chunks") and not self.chunks:
                self.plan()
            if snapshot.values and not snapshot.next and snapshot.values.get("result"):
                return self.finish(snapshot.values["result"])
            if snapshot.values:
                initial = None
                self.emit_progress("恢复分段分析", "复用已完成分片，从失败或中断位置继续")
            else:
                initial = {"packets": []}
            result = workflow.invoke(initial, config)
            return self.finish(result["result"])
