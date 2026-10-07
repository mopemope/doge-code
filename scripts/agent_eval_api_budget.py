"""Opt-in evaluation guard for pinned, text-only OpenAI Chat Completions.

Reservations are permanent worst-case upper bounds, never measured charges.
No request bodies, credentials or provider bodies are retained in artifacts.
"""
import datetime
import hmac
import http.client
import json
import secrets
import threading
import tomllib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

# Published Standard prices/context/output bounds, checked 2026-10-07.
# No aliases, unknown providers, multimodal or service-tier price inference.
PROFILES = {
    "gpt-4o-mini-2024-07-18": (128000, 16384, 150, 600),
    "gpt-4.1-mini-2025-04-14": (1047576, 32768, 400, 1600),
}
MAX_BODY = 4 * 1024 * 1024


def validate_budget(raw, model, provider, variants):
    if raw is None:
        return None
    keys = {"max_cost_micro_usd", "max_requests", "max_requests_per_run", "max_output_tokens",
            "input_nano_usd_per_token", "output_nano_usd_per_token", "pricing_reviewed_on"}
    if not isinstance(raw, dict) or set(raw) != keys:
        raise ValueError("api_budget requires exactly the documented fields")
    if provider != "openai-compatible" or model not in PROFILES:
        raise ValueError("api_budget requires a supported pinned model and openai-compatible provider")
    for key in keys - {"pricing_reviewed_on"}:
        if type(raw[key]) is not int or raw[key] <= 0:
            raise ValueError(f"api_budget {key} must be a positive integer")
    if raw["pricing_reviewed_on"] != datetime.datetime.now(datetime.timezone.utc).date().isoformat():
        raise ValueError("api_budget pricing_reviewed_on must confirm today's UTC pricing")
    context, output, input_rate, output_rate = PROFILES[model]
    if (raw["input_nano_usd_per_token"] < input_rate
            or raw["output_nano_usd_per_token"] < output_rate):
        raise ValueError("api_budget prices are below the known Standard price floors")
    if raw["max_output_tokens"] > output:
        raise ValueError("api_budget output cap exceeds the model limit")
    reservation = context * raw["input_nano_usd_per_token"] + raw["max_output_tokens"] * raw["output_nano_usd_per_token"]
    if reservation * raw["max_requests"] > raw["max_cost_micro_usd"] * 1000:
        raise ValueError("api_budget worst-case request reservations exceed the total cost ceiling")
    for variant in variants:
        if not variant.get("config"):
            raise ValueError("api_budget requires an explicit credential-free variant config")
        config = tomllib.loads(Path(variant["config"]).read_text(encoding="utf-8"))
        if config.get("api_key") or config.get("mcp_servers"):
            raise ValueError("api_budget config must not contain credentials or remote MCP servers")
        llm = config.get("llm", {})
        if not isinstance(llm, dict) or type(llm.get("max_retries")) is not int or llm["max_retries"] != 0:
            raise ValueError("api_budget requires explicit llm.max_retries=0")
        if config.get("base_url", "https://api.openai.com/v1").rstrip("/") != "https://api.openai.com/v1":
            raise ValueError("api_budget cannot override a nonofficial configured base URL")
        # Only the existing official endpoint may be rerouted. Never silently
        # override a user's custom provider or duplicate a CLI option.
        argv = variant["agent_command"]
        for index, arg in enumerate(argv):
            if arg == "--base-url":
                if index + 1 == len(argv) or argv[index + 1].rstrip("/") != "https://api.openai.com/v1":
                    raise ValueError("api_budget requires the official base URL")
            elif arg.startswith("--base-url=") and arg.split("=", 1)[1].rstrip("/") != "https://api.openai.com/v1":
                raise ValueError("api_budget requires the official base URL")
    return dict(raw, model=model, context_tokens=context, reservation_nano_usd=reservation,
                planned_upper_nano_usd=reservation * raw["max_requests"])


class BudgetStopped(Exception):
    pass


class ApiBudgetGuard:
    def __init__(self, budget, api_key, *, upstream=None, timeout=60):
        if not api_key:
            raise ValueError("api_budget requires OPENAI_API_KEY in the parent environment")
        self.budget = budget
        self._api_key = api_key
        self._token = secrets.token_urlsafe(32)
        self._upstream = upstream or self._official_request
        self._timeout = timeout
        self._lock = threading.Lock()
        self._requests = 0
        self._reserved = 0
        self._run_requests = 0
        self._active = False
        self._observed_prompt = 0
        self._observed_completion = 0
        self._stopped = None
        self._pending_stop = threading.Event()
        self._pending_reason = None
        self._server = None
        self._thread = None

    def environment(self, env):
        result = dict(env)
        result["OPENAI_API_KEY"] = self._token
        result["OPENAI_BASE_URL"] = self.url
        # Loopback must bypass inherited HTTP proxies.
        result["NO_PROXY"] = result["no_proxy"] = "127.0.0.1,localhost"
        return result

    def variant(self, variant):
        result = dict(variant)
        argv, skip = [], False
        for arg in variant["agent_command"]:
            if skip:
                skip = False
            elif arg == "--base-url":
                skip = True
            elif not arg.startswith("--base-url="):
                argv.append(arg)
        result["agent_command"] = argv + ["--base-url", self.url]
        return result

    @property
    def stopped(self):
        with self._lock:
            return self._stopped or (self._pending_reason if self._pending_stop.is_set() else None)

    def begin_run(self):
        with self._lock:
            if self._stopped or self._pending_stop.is_set():
                raise BudgetStopped(self._stopped or self._pending_reason)
            self._active = True
            self._run_requests = 0

    def end_run(self):
        # Wait for any already-reserved upstream call before advancing the
        # matrix. Disconnects never refund the request reservation.
        with self._lock:
            self._active = False
            return self._snapshot()

    def stop(self, reason):
        # Sticky and nonblocking, including a malformed/concurrent local
        # request arriving while another upstream call owns the mutex.
        if not self._pending_stop.is_set():
            self._pending_reason = reason
            self._pending_stop.set()

    def _snapshot(self):
        return {"requests": self._requests, "run_requests": self._run_requests,
                "reserved_upper_nano_usd": self._reserved,
                "stop_reason": self._stopped or (self._pending_reason if self._pending_stop.is_set() else None),
                "observed_prompt_tokens": self._observed_prompt,
                "observed_completion_tokens": self._observed_completion,
                "actual_cost": None, "reservation_is_measured_usage": False}

    def snapshot(self):
        with self._lock:
            return self._snapshot()

    def _prepare(self, request):
        if not isinstance(request, dict) or request.get("model") != self.budget["model"]:
            raise BudgetStopped("unknown_model")
        allowed = {"model", "messages", "tools", "tool_choice", "temperature", "reasoning_effort",
                   "stream", "max_tokens", "max_completion_tokens", "n"}
        if set(request) - allowed or (request.get("stream") is not None and request.get("stream") is not False) or type(request.get("n", 1)) is not int or request.get("n", 1) != 1:
            raise BudgetStopped("unsupported_request")
        messages = request.get("messages")
        if not isinstance(messages, list) or not messages:
            raise BudgetStopped("unsupported_messages")
        for message in messages:
            if (not isinstance(message, dict)
                    or set(message) - {"role", "content", "tool_calls", "tool_call_id", "name"}
                    or message.get("content") is not None and not isinstance(message["content"], str)):
                raise BudgetStopped("unsupported_multimodal_or_provider_state")
        tools = request.get("tools") or []
        if not isinstance(tools, list) or any(not isinstance(tool, dict) or tool.get("type") != "function" for tool in tools):
            raise BudgetStopped("unsupported_tools")
        present = [key for key in ("max_tokens", "max_completion_tokens") if request.get(key) is not None]
        if len(present) > 1:
            raise BudgetStopped("conflicting_output_caps")
        cap = request[present[0]] if present else self.budget["max_output_tokens"]
        if type(cap) is not int or not 0 < cap <= self.budget["max_output_tokens"]:
            raise BudgetStopped("invalid_output_cap")
        prepared = dict(request)
        if not present:
            prepared.pop("max_tokens", None)
            prepared["max_completion_tokens"] = cap
        prepared["service_tier"] = "default"
        return prepared, cap

    def request(self, request):
        # Reject overlapping requests; do not let the child turn serial
        # assumptions into a hidden queue of already-started billable work.
        if not self._lock.acquire(blocking=False):
            self.stop("concurrent_request")
            raise BudgetStopped("concurrent_request")
        try:
            if self._stopped or self._pending_stop.is_set():
                raise BudgetStopped(self._stopped or self._pending_reason)
            try:
                prepared, cap = self._prepare(request)
                if not self._active:
                    raise BudgetStopped("outside_agent_run")
                if self._requests >= self.budget["max_requests"] or self._run_requests >= self.budget["max_requests_per_run"]:
                    raise BudgetStopped("request_limit")
                reservation = self.budget["reservation_nano_usd"]
                if self._reserved + reservation > self.budget["max_cost_micro_usd"] * 1000:
                    raise BudgetStopped("cost_limit")
                self._requests += 1
                self._run_requests += 1
                self._reserved += reservation
                # All uncertainty retains the full reservation, stops the
                # whole matrix, and never retries or discounts absent usage.
                try:
                    status, body = self._upstream(prepared)
                except Exception:
                    raise BudgetStopped("upstream_transport_or_timeout") from None
                if status != 200:
                    raise BudgetStopped(f"upstream_http_{status}")
                try:
                    response = json.loads(body)
                except (ValueError, UnicodeError):
                    raise BudgetStopped("malformed_response") from None
                if not isinstance(response, dict) or response.get("model") != self.budget["model"]:
                    raise BudgetStopped("unknown_response_model")
                if response.get("service_tier") != "default":
                    raise BudgetStopped("unknown_or_nonstandard_service_tier")
                usage = response.get("usage")
                fields = ("prompt_tokens", "completion_tokens", "total_tokens")
                if (not isinstance(usage, dict)
                        or any(type(usage.get(key)) is not int or usage[key] < 0 for key in fields)
                        or usage["prompt_tokens"] > self.budget["context_tokens"]
                        or usage["completion_tokens"] > cap
                        or usage["total_tokens"] != usage["prompt_tokens"] + usage["completion_tokens"]):
                    raise BudgetStopped("unknown_or_invalid_usage")
                self._observed_prompt += usage["prompt_tokens"]
                self._observed_completion += usage["completion_tokens"]
                choices = response.get("choices")
                if (not isinstance(choices, list) or len(choices) != 1
                        or not isinstance(choices[0], dict)
                        or choices[0].get("finish_reason") not in ("stop", "tool_calls", "length", "content_filter")):
                    raise BudgetStopped("invalid_response_outcome")
                if choices[0]["finish_reason"] in ("length", "content_filter"):
                    self._stopped = "output_limit" if choices[0]["finish_reason"] == "length" else "content_filter"
                return body
            except BudgetStopped as error:
                self._stopped = str(error)
                raise
        finally:
            self._lock.release()

    def _official_request(self, request):
        connection = http.client.HTTPSConnection("api.openai.com", timeout=self._timeout)
        try:
            connection.request("POST", "/v1/chat/completions", body=json.dumps(request).encode(),
                               headers={"Authorization": "Bearer " + self._api_key,
                                        "Content-Type": "application/json", "Accept-Encoding": "identity"})
            response = connection.getresponse()
            body = response.read(MAX_BODY + 1)
            if len(body) > MAX_BODY:
                raise BudgetStopped("upstream_response_too_large")
            return response.status, body
        finally:
            connection.close()

    def start(self):
        guard = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass  # Never log local auth, request paths or provider bodies.

            def setup(self):
                super().setup()
                self.connection.settimeout(guard._timeout)

            def do_POST(self):
                authenticated = False
                try:
                    if not hmac.compare_digest(
                            self.headers.get("Authorization", ""), "Bearer " + guard._token):
                        raise BudgetStopped("unauthorized_local_route")
                    authenticated = True
                    if self.path != "/v1/chat/completions":
                        raise BudgetStopped("unsupported_local_route")
                    size = int(self.headers.get("Content-Length", "0"))
                    if not 0 < size <= MAX_BODY:
                        raise BudgetStopped("invalid_request_size")
                    self.connection.settimeout(guard._timeout)
                    request = json.loads(self.rfile.read(size))
                    body = guard.request(request)
                    status = 200
                except (BudgetStopped, ValueError, OSError):
                    if authenticated:
                        guard.stop("invalid_local_request")
                    status = 403  # Non-retryable; no error/body/credential reflection.
                    body = b'{"error":{"code":"eval_api_budget_stopped","message":"Evaluation API budget guard stopped this request."}}'
                try:
                    self.send_response(status)
                    self.send_header("Content-Type", "application/json")
                    self.send_header("Content-Length", str(len(body)))
                    self.end_headers()
                    self.wfile.write(body)
                except OSError:
                    pass

        self._server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self._server.daemon_threads = True
        self._thread = threading.Thread(target=self._server.serve_forever, daemon=True)
        self._thread.start()
        return self

    @property
    def url(self):
        return f"http://127.0.0.1:{self._server.server_port}/v1"

    def close(self):
        self.end_run()
        self._server.shutdown()
        self._server.server_close()
        self._thread.join()
