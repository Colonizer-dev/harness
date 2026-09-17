"""Smoke test for a Headroom bundle, run with the bundle's own Python.

Starts `headroom proxy` the way the claude-code runner does in a colony (same flags and environment),
in front of a stand-in upstream, and checks what a colony depends on:

- it becomes healthy;
- a request with a large JSON tool result reaches upstream smaller, with nothing in it lost;
- a streamed response comes back event for event;
- nothing is served from a cache: every request reaches upstream.

    <bundle>/python/bin/python3 scripts/headroom-bundle/smoke.py <bundle>

Exits non-zero on any failure.
"""

import http.server
import json
import os
import socket
import subprocess
import sys
import threading
import time
import urllib.request

# Keep in step with how the claude-code runner starts Headroom in a colony (modules/agents/claude-code).
HEADROOM_ENV = {
    "HEADROOM_OFFLINE": "1",
    "HEADROOM_BEACON": "off",
    "DO_NOT_TRACK": "1",
    "HEADROOM_NO_SUBSCRIPTION_TRACKING": "1",
    "HEADROOM_DISABLE_KOMPRESS": "1",
    "LITELLM_LOCAL_MODEL_COST_MAP": "True",
    "HF_HUB_OFFLINE": "1",
    "TRANSFORMERS_OFFLINE": "1",
    "PYTHONDONTWRITEBYTECODE": "1",
}

STREAM_EVENTS = [
    ("message_start", {"type": "message_start", "message": {"id": "msg_s", "type": "message", "role": "assistant", "model": "claude-sonnet-5", "content": [], "stop_reason": None, "stop_sequence": None, "usage": {"input_tokens": 1, "output_tokens": 1}}}),
    ("content_block_start", {"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
    ("content_block_delta", {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "streamed "}}),
    ("content_block_delta", {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "through headroom"}}),
    ("content_block_stop", {"type": "content_block_stop", "index": 0}),
    ("message_delta", {"type": "message_delta", "delta": {"stop_reason": "end_turn", "stop_sequence": None}, "usage": {"output_tokens": 4}}),
    ("message_stop", {"type": "message_stop"}),
]

received = []


class Upstream(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        body = self.rfile.read(int(self.headers.get("content-length") or 0))
        request = json.loads(body)
        received.append({"path": self.path, "bytes": len(body), "request": request})
        if request.get("stream"):
            self.send_response(200)
            self.send_header("content-type", "text/event-stream")
            self.end_headers()
            for name, data in STREAM_EVENTS:
                self.wfile.write(f"event: {name}\ndata: {json.dumps(data)}\n\n".encode())
                self.wfile.flush()
            return
        out = json.dumps({"id": "msg_j", "type": "message", "role": "assistant", "model": "claude-sonnet-5", "content": [{"type": "text", "text": "ok"}], "stop_reason": "end_turn", "stop_sequence": None, "usage": {"input_tokens": 1, "output_tokens": 1}}).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(out)))
        self.end_headers()
        self.wfile.write(out)

    def do_GET(self):
        self.send_response(200)
        self.end_headers()

    def log_message(self, *_):
        pass


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def fail(message, log_path=None):
    print(f"FAIL: {message}")
    if log_path and os.path.exists(log_path):
        print("--- proxy log (tail) ---")
        print("".join(open(log_path, encoding="utf-8", errors="replace").readlines()[-30:]))
    sys.exit(1)


def post(url, body, stream=False):
    data = json.dumps(body).encode()
    req = urllib.request.Request(url, data=data, method="POST", headers={"content-type": "application/json", "x-api-key": "placeholder", "anthropic-version": "2023-06-01"})
    with urllib.request.urlopen(req, timeout=120) as res:
        return res.status, res.read().decode(), len(data)


def main():
    bundle = os.path.abspath(sys.argv[1])
    python = os.path.join(bundle, "python", "bin", "python3")
    upstream_port, proxy_port = free_port(), free_port()
    server = http.server.ThreadingHTTPServer(("127.0.0.1", upstream_port), Upstream)
    threading.Thread(target=server.serve_forever, daemon=True).start()

    log_path = os.path.join(os.environ.get("TMPDIR", "/tmp"), "headroom-smoke.log")
    env = {**os.environ, **HEADROOM_ENV, "HOME": os.environ.get("TMPDIR", "/tmp")}
    started = time.time()
    with open(log_path, "w") as log:
        proxy = subprocess.Popen(
            [python, "-m", "headroom.cli", "proxy", "--host", "127.0.0.1", "--port", str(proxy_port),
             "--anthropic-api-url", f"http://127.0.0.1:{upstream_port}", "--stateless", "--no-cache"],
            env=env, stdout=log, stderr=subprocess.STDOUT)
    try:
        base = f"http://127.0.0.1:{proxy_port}"
        for _ in range(180):
            if proxy.poll() is not None:
                fail(f"proxy exited with {proxy.returncode} before it was healthy", log_path)
            try:
                with urllib.request.urlopen(f"{base}/health", timeout=2) as res:
                    if res.status == 200:
                        break
            except OSError:
                time.sleep(0.5)
        else:
            fail("proxy not healthy within 90 s", log_path)
        print(f"healthy after {time.time() - started:.1f} s")

        rows = [{"id": i, "level": "error" if i % 50 == 0 else "info", "message": f"timeout (order {1000 + i})" if i % 50 == 0 else "request handled", "latency_ms": 20 + i % 7} for i in range(400)]
        body = {"model": "claude-sonnet-5", "max_tokens": 256, "messages": [
            {"role": "user", "content": "why are checkouts failing?"},
            {"role": "assistant", "content": [{"type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {"command": "cat logs.json"}}]},
            {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": json.dumps(rows, indent=2)}]},
        ]}
        status, _, sent = post(f"{base}/v1/messages", body)
        if status != 200:
            fail(f"JSON request returned {status}", log_path)
        seen = received[-1]
        tool_result = json.dumps(seen["request"]["messages"][-1]["content"])
        kept = sum(1 for i in range(400) if i % 50 == 0 and f"order {1000 + i}" in tool_result)
        print(f"JSON request: sent {sent} bytes, upstream received {seen['bytes']} ({100 - seen['bytes'] * 100 // sent}% smaller), error rows kept {kept}/8")
        if seen["bytes"] >= sent * 0.8:
            fail("the JSON tool result was not compressed", log_path)
        if kept != 8:
            fail("compression lost error rows", log_path)

        count = len(received)
        post(f"{base}/v1/messages", body)
        if len(received) != count + 1:
            fail("an identical request was answered without reaching upstream (a cache is on)", log_path)
        print("repeat request reached upstream: no response cache")

        status, text, _ = post(f"{base}/v1/messages", {**body, "stream": True}, stream=True)
        events = [line[len("event: "):] for line in text.splitlines() if line.startswith("event: ")]
        if status != 200 or events != [name for name, _ in STREAM_EVENTS] or "through headroom" not in text:
            fail(f"streamed response changed on the way through: status {status}, events {events}", log_path)
        print(f"streamed response intact: {len(events)} events")
        print("OK")
    finally:
        proxy.terminate()
        try:
            proxy.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proxy.kill()
        server.shutdown()


if __name__ == "__main__":
    main()
