#!/usr/bin/env python3
"""
Simple upstream HTTP server for testing the adaptive concurrency WASM plugin.

Environment variables:
  PORT          - Listen port (default: 8080)
  LATENCY_MS    - Base response latency in milliseconds (default: 5)
  SERVER_NAME   - Name for identification in responses (default: hostname)
  SLOW_AFTER    - Number of requests after which to add extra latency (default: 0 = never)
  SLOW_LATENCY_MS - Extra latency to add after SLOW_AFTER requests (default: 500)
  CONTROL_PORT  - Port for the control API (default: 8081)

Control API (on CONTROL_PORT):
  POST /control/latency    {"latency_ms": 500}   - Set base latency
  POST /control/reset                             - Reset request counter
  GET  /control/stats                             - Get request stats
"""

import http.server
import json
import os
import socket
import time
import threading

PORT = int(os.environ.get("PORT", "8080"))
CONTROL_PORT = int(os.environ.get("CONTROL_PORT", "8081"))
SERVER_NAME = os.environ.get("SERVER_NAME", socket.gethostname())
SLOW_AFTER = int(os.environ.get("SLOW_AFTER", "0"))
SLOW_LATENCY_MS = int(os.environ.get("SLOW_LATENCY_MS", "500"))

# Mutable state protected by lock
state_lock = threading.Lock()
state = {
    "latency_ms": int(os.environ.get("LATENCY_MS", "5")),
    "request_count": 0,
    "total_requests": 0,
}


class TrafficHandler(http.server.BaseHTTPRequestHandler):
    """Handles actual traffic requests (fronted by Envoy)."""

    def do_GET(self):
        start = time.time()

        with state_lock:
            state["request_count"] += 1
            state["total_requests"] += 1
            current_count = state["request_count"]
            latency_ms = state["latency_ms"]

        # Add extra latency if SLOW_AFTER threshold reached
        if SLOW_AFTER > 0 and current_count > SLOW_AFTER:
            latency_ms += SLOW_LATENCY_MS

        if latency_ms > 0:
            time.sleep(latency_ms / 1000.0)

        elapsed = (time.time() - start) * 1000

        response = {
            "server": SERVER_NAME,
            "request_number": current_count,
            "latency_ms": round(elapsed, 2),
            "configured_latency_ms": latency_ms,
        }

        body = json.dumps(response).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("X-Server-Name", SERVER_NAME)
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        self.do_GET()

    def log_message(self, format, *args):
        pass


class ControlHandler(http.server.BaseHTTPRequestHandler):
    """Control API for dynamically adjusting server behavior during tests."""

    def do_GET(self):
        if self.path == "/control/stats":
            with state_lock:
                stats = {
                    "server": SERVER_NAME,
                    "latency_ms": state["latency_ms"],
                    "request_count": state["request_count"],
                    "total_requests": state["total_requests"],
                }
            self._respond(200, stats)
        else:
            self._respond(404, {"error": "not found"})

    def do_POST(self):
        content_length = int(self.headers.get("Content-Length", 0))
        body = self.rfile.read(content_length) if content_length > 0 else b""

        if self.path == "/control/latency":
            try:
                data = json.loads(body) if body else {}
                new_latency = int(data.get("latency_ms", 5))
                with state_lock:
                    old = state["latency_ms"]
                    state["latency_ms"] = new_latency
                print(f"[{SERVER_NAME}] Latency changed: {old}ms -> {new_latency}ms")
                self._respond(200, {"ok": True, "latency_ms": new_latency})
            except (json.JSONDecodeError, ValueError) as e:
                self._respond(400, {"error": str(e)})

        elif self.path == "/control/reset":
            with state_lock:
                state["request_count"] = 0
            print(f"[{SERVER_NAME}] Request counter reset")
            self._respond(200, {"ok": True})

        else:
            self._respond(404, {"error": "not found"})

    def _respond(self, code, data):
        body = json.dumps(data).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, format, *args):
        pass


def main():
    traffic_server = http.server.HTTPServer(("0.0.0.0", PORT), TrafficHandler)
    control_server = http.server.HTTPServer(("0.0.0.0", CONTROL_PORT), ControlHandler)

    print(f"Upstream server '{SERVER_NAME}'")
    print(f"  Traffic port: {PORT}")
    print(f"  Control port: {CONTROL_PORT}")
    print(f"  Base latency: {state['latency_ms']}ms")
    if SLOW_AFTER > 0:
        print(f"  Will add {SLOW_LATENCY_MS}ms latency after {SLOW_AFTER} requests")

    threading.Thread(target=control_server.serve_forever, daemon=True).start()
    traffic_server.serve_forever()


if __name__ == "__main__":
    main()
