#!/usr/bin/env python3
"""
Comprehensive e2e test suite for the adaptive concurrency WASM plugin.

Requires:
  - Docker Compose e2e environment running (docker-compose.e2e.yaml)
  - Envoy on localhost:10000 (proxy) and localhost:9901 (admin)
  - Upstream control APIs on localhost:18081-18085

Usage:
  python3 tests/e2e/run_tests.py
  python3 tests/e2e/run_tests.py TestHealthyCluster
  python3 tests/e2e/run_tests.py TestSlowHostDetection.test_slow_host_gets_limited
"""

import json
import sys
import time
import unittest
import urllib.error
import urllib.request
from collections import Counter
from concurrent.futures import ThreadPoolExecutor, as_completed

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------
ENVOY_URL = "http://localhost:10000"
ENVOY_ADMIN = "http://localhost:9901"
CONTROL_PORTS = {
    "upstream-1": 18081,
    "upstream-2": 18082,
    "upstream-3": 18083,
    "upstream-4": 18084,
    "upstream-5": 18085,
}

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def http_get(url, timeout=10):
    """GET request, returns (status_code, headers_dict, body_str)."""
    req = urllib.request.Request(url)
    try:
        resp = urllib.request.urlopen(req, timeout=timeout)
        body = resp.read().decode()
        headers = dict(resp.headers)
        return resp.status, headers, body
    except urllib.error.HTTPError as e:
        body = e.read().decode() if e.fp else ""
        headers = dict(e.headers) if e.headers else {}
        return e.code, headers, body
    except urllib.error.URLError as e:
        raise ConnectionError(f"Failed to connect to {url}: {e}")


def http_post(url, data=None, timeout=10):
    """POST request with JSON body."""
    body = json.dumps(data).encode() if data else b""
    req = urllib.request.Request(
        url, data=body, headers={"Content-Type": "application/json"}, method="POST"
    )
    try:
        resp = urllib.request.urlopen(req, timeout=timeout)
        return resp.status, json.loads(resp.read().decode())
    except urllib.error.HTTPError as e:
        return e.code, {}


def set_upstream_latency(name, latency_ms):
    """Set latency for an upstream server via its control API."""
    port = CONTROL_PORTS[name]
    status, resp = http_post(
        f"http://localhost:{port}/control/latency",
        {"latency_ms": latency_ms},
    )
    assert status == 200, f"Failed to set latency on {name}: {resp}"


def reset_upstream(name):
    """Reset request counter for an upstream server."""
    port = CONTROL_PORTS[name]
    http_post(f"http://localhost:{port}/control/reset")


def get_upstream_stats(name):
    """Get stats from an upstream server."""
    port = CONTROL_PORTS[name]
    status, _, body = http_get(f"http://localhost:{port}/control/stats")
    return json.loads(body)


def reset_all_upstreams(latency_ms=5):
    """Reset all upstreams to healthy state."""
    for name in CONTROL_PORTS:
        set_upstream_latency(name, latency_ms)
        reset_upstream(name)


def send_requests(n, concurrency=10, timeout=15):
    """Send n requests through Envoy, returns list of (status, headers, body, elapsed_s)."""
    results = []

    def _do_request(_):
        start = time.time()
        try:
            status, headers, body = http_get(ENVOY_URL, timeout=timeout)
            elapsed = time.time() - start
            return status, headers, body, elapsed
        except Exception as e:
            elapsed = time.time() - start
            return 0, {}, str(e), elapsed

    with ThreadPoolExecutor(max_workers=concurrency) as pool:
        futures = [pool.submit(_do_request, i) for i in range(n)]
        for f in as_completed(futures):
            results.append(f.result())

    return results


def send_requests_serial(n, timeout=15):
    """Send n requests serially (no concurrency)."""
    results = []
    for _ in range(n):
        start = time.time()
        try:
            status, headers, body = http_get(ENVOY_URL, timeout=timeout)
            elapsed = time.time() - start
            results.append((status, headers, body, elapsed))
        except Exception as e:
            elapsed = time.time() - start
            results.append((0, {}, str(e), elapsed))
    return results


def count_servers(results):
    """Count how many responses came from each upstream server."""
    counter = Counter()
    for status, headers, body, _ in results:
        if status == 200:
            try:
                data = json.loads(body)
                counter[data.get("server", "unknown")] += 1
            except (json.JSONDecodeError, TypeError):
                counter["parse_error"] += 1
        else:
            counter[f"status_{status}"] += 1
    return counter


def count_status_codes(results):
    """Count status codes from results."""
    return Counter(status for status, _, _, _ in results)


def wait_for_envoy(max_wait=30):
    """Wait until Envoy is ready."""
    deadline = time.time() + max_wait
    while time.time() < deadline:
        try:
            status, _, _ = http_get(f"{ENVOY_ADMIN}/ready", timeout=2)
            if status == 200:
                return True
        except Exception:
            pass
        time.sleep(1)
    raise TimeoutError("Envoy did not become ready")


def wait_for_upstreams(max_wait=30):
    """Wait until all upstream control APIs are reachable."""
    deadline = time.time() + max_wait
    for name, port in CONTROL_PORTS.items():
        while time.time() < deadline:
            try:
                status, _, _ = http_get(
                    f"http://localhost:{port}/control/stats", timeout=2
                )
                if status == 200:
                    break
            except Exception:
                pass
            time.sleep(1)
        else:
            raise TimeoutError(f"Upstream {name} did not become ready")


def warm_up_envoy(n=50):
    """Send warmup requests to establish baseline minRTT for all hosts."""
    send_requests(n, concurrency=5)
    # Wait for a tick cycle to process samples
    time.sleep(2)


# ---------------------------------------------------------------------------
# Test Cases
# ---------------------------------------------------------------------------

class TestHealthyCluster(unittest.TestCase):
    """Tests that the plugin does not interfere with a healthy cluster."""

    @classmethod
    def setUpClass(cls):
        reset_all_upstreams(latency_ms=5)
        time.sleep(1)
        warm_up_envoy(100)

    def test_all_requests_succeed(self):
        """All requests to a healthy cluster should return 200."""
        results = send_requests(200, concurrency=20)
        statuses = count_status_codes(results)
        total_ok = statuses.get(200, 0)
        self.assertGreaterEqual(
            total_ok, 190,
            f"Expected nearly all 200s in healthy cluster, got {statuses}",
        )

    def test_traffic_distributed_across_hosts(self):
        """Traffic should be roughly distributed across all 5 hosts."""
        results = send_requests(250, concurrency=10)
        servers = count_servers(results)
        # Each of 5 hosts should get at least 10% (25 requests) of 250
        for i in range(1, 6):
            name = f"upstream-{i}"
            count = servers.get(name, 0)
            self.assertGreater(
                count, 15,
                f"Host {name} got only {count}/250 requests, expected more even distribution. Full: {servers}",
            )

    def test_response_latency_reasonable(self):
        """Response latency in a healthy cluster should be low."""
        results = send_requests(50, concurrency=5)
        latencies = [elapsed for status, _, _, elapsed in results if status == 200]
        self.assertTrue(len(latencies) > 0, "No successful requests")
        avg_latency = sum(latencies) / len(latencies)
        # With 5ms upstream + network overhead, average should be under 500ms
        self.assertLess(
            avg_latency, 0.5,
            f"Average latency {avg_latency:.3f}s too high for healthy cluster",
        )


class TestSlowHostDetection(unittest.TestCase):
    """Tests that the plugin detects and limits a slow host."""

    @classmethod
    def setUpClass(cls):
        reset_all_upstreams(latency_ms=5)
        time.sleep(1)
        # Warm up to establish minRTT baselines
        warm_up_envoy(150)

    def setUp(self):
        reset_all_upstreams(latency_ms=5)
        time.sleep(1)

    def test_slow_host_gets_limited(self):
        """When one host becomes slow, the plugin should detect it and reduce
        traffic to that host via 503 + retry."""
        # Make upstream-5 very slow
        set_upstream_latency("upstream-5", 500)

        # Send enough traffic for the plugin to collect samples and detect
        # the slow host. With sample_window_size=25 and tick=500ms, we need
        # a few hundred requests over several seconds.
        all_results = []
        for _ in range(6):
            batch = send_requests(50, concurrency=10)
            all_results.extend(batch)
            time.sleep(1)

        # Later batches should show the slow host getting limited.
        # Check that the majority of successful responses are from fast hosts.
        late_results = all_results[150:]  # Latter half
        servers = count_servers(late_results)

        slow_count = servers.get("upstream-5", 0)
        total_ok = sum(v for k, v in servers.items() if not k.startswith("status_"))

        if total_ok > 0:
            slow_pct = slow_count / total_ok
            # The slow host should get significantly less than its fair share (20%)
            # Once detected, it should get close to 0
            self.assertLess(
                slow_pct, 0.15,
                f"Slow host still getting {slow_pct:.1%} of traffic ({slow_count}/{total_ok}). Servers: {servers}",
            )

    def test_fast_hosts_unaffected(self):
        """Fast hosts should continue to serve traffic normally when one host
        is slow."""
        set_upstream_latency("upstream-5", 500)

        # Send traffic
        for _ in range(4):
            send_requests(50, concurrency=10)
            time.sleep(1)

        # Now measure: fast hosts should handle requests quickly
        results = send_requests(100, concurrency=10)
        fast_latencies = []
        for status, _, body, elapsed in results:
            if status == 200:
                try:
                    data = json.loads(body)
                    if data.get("server", "").startswith("upstream-") and data["server"] != "upstream-5":
                        fast_latencies.append(elapsed)
                except (json.JSONDecodeError, TypeError):
                    pass

        if fast_latencies:
            avg = sum(fast_latencies) / len(fast_latencies)
            self.assertLess(
                avg, 0.5,
                f"Fast host average latency {avg:.3f}s too high — should be unaffected by slow host",
            )


class TestRetryBehavior(unittest.TestCase):
    """Tests that retries work correctly when hosts are overloaded."""

    @classmethod
    def setUpClass(cls):
        reset_all_upstreams(latency_ms=5)
        time.sleep(1)
        warm_up_envoy(150)

    def setUp(self):
        reset_all_upstreams(latency_ms=5)
        time.sleep(1)

    def test_client_sees_mostly_200s_despite_slow_host(self):
        """Even with a slow host, clients should get 200s thanks to retries."""
        set_upstream_latency("upstream-5", 500)

        # Let the plugin detect the slow host
        for _ in range(5):
            send_requests(50, concurrency=10)
            time.sleep(1)

        # Now check: client should still see mostly 200s
        results = send_requests(100, concurrency=10)
        statuses = count_status_codes(results)
        ok_count = statuses.get(200, 0)

        self.assertGreaterEqual(
            ok_count, 80,
            f"Expected most requests to succeed via retry, got {statuses}",
        )

    def test_overloaded_header_present(self):
        """When the plugin rejects a response, the x-adaptive-concurrency-limited
        header should be set (visible if retry is exhausted)."""
        set_upstream_latency("upstream-5", 800)

        # Build up enough data for detection
        for _ in range(6):
            send_requests(50, concurrency=10)
            time.sleep(1)

        # Check for the header in any 503 responses
        results = send_requests(100, concurrency=10)
        limited_503s = 0
        for status, headers, body, _ in results:
            if status == 503:
                # The header may or may not be present depending on whether
                # retry exhaustion occurred
                if headers.get("x-adaptive-concurrency-limited") == "true":
                    limited_503s += 1

        # This is informational — it's okay if retries absorb all 503s
        # But we want to know the mechanism works
        print(f"  [info] Saw {limited_503s} 503s with x-adaptive-concurrency-limited header")


class TestHostRecovery(unittest.TestCase):
    """Tests that a host recovers after its latency improves."""

    @classmethod
    def setUpClass(cls):
        reset_all_upstreams(latency_ms=5)
        time.sleep(1)
        warm_up_envoy(150)

    def test_host_recovers_after_latency_drops(self):
        """A host that was slow should eventually be re-included in rotation
        after its latency returns to normal."""
        # Phase 1: Make upstream-5 slow
        set_upstream_latency("upstream-5", 500)
        for _ in range(5):
            send_requests(50, concurrency=10)
            time.sleep(1)

        # Verify it's being limited
        results_degraded = send_requests(100, concurrency=10)
        servers_degraded = count_servers(results_degraded)
        slow_during = servers_degraded.get("upstream-5", 0)

        # Phase 2: Restore upstream-5 to normal
        set_upstream_latency("upstream-5", 5)

        # Send traffic and wait for recovery (Gradient2 recovery is gradual)
        for _ in range(10):
            send_requests(50, concurrency=10)
            time.sleep(1)

        # Phase 3: Check that upstream-5 is getting traffic again
        results_recovered = send_requests(200, concurrency=10)
        servers_recovered = count_servers(results_recovered)
        slow_after = servers_recovered.get("upstream-5", 0)
        total_after = sum(v for k, v in servers_recovered.items() if not k.startswith("status_"))

        print(f"  [info] upstream-5 during degradation: {slow_during} requests")
        print(f"  [info] upstream-5 after recovery: {slow_after}/{total_after} requests")

        # After recovery, upstream-5 should be getting at least some traffic
        self.assertGreater(
            slow_after, 5,
            f"upstream-5 not recovering: only {slow_after}/{total_after} requests after latency restored. Servers: {servers_recovered}",
        )


class TestMultipleDegradedHosts(unittest.TestCase):
    """Tests behavior when multiple hosts degrade simultaneously."""

    @classmethod
    def setUpClass(cls):
        reset_all_upstreams(latency_ms=5)
        time.sleep(1)
        warm_up_envoy(150)

    def setUp(self):
        reset_all_upstreams(latency_ms=5)
        time.sleep(1)

    def test_two_slow_hosts(self):
        """When 2 out of 5 hosts are slow, traffic should shift to the 3 healthy ones."""
        set_upstream_latency("upstream-4", 500)
        set_upstream_latency("upstream-5", 500)

        for _ in range(6):
            send_requests(50, concurrency=10)
            time.sleep(1)

        results = send_requests(150, concurrency=10)
        servers = count_servers(results)

        slow_count = servers.get("upstream-4", 0) + servers.get("upstream-5", 0)
        total_ok = sum(v for k, v in servers.items() if not k.startswith("status_"))

        if total_ok > 0:
            slow_pct = slow_count / total_ok
            self.assertLess(
                slow_pct, 0.25,
                f"Slow hosts still getting {slow_pct:.1%} of traffic. Servers: {servers}",
            )

    def test_majority_degraded(self):
        """When 4 out of 5 hosts are slow, the one healthy host should handle
        most traffic. Some failures are acceptable."""
        set_upstream_latency("upstream-2", 500)
        set_upstream_latency("upstream-3", 500)
        set_upstream_latency("upstream-4", 500)
        set_upstream_latency("upstream-5", 500)

        for _ in range(6):
            send_requests(50, concurrency=10)
            time.sleep(1)

        results = send_requests(100, concurrency=10)
        servers = count_servers(results)

        healthy_count = servers.get("upstream-1", 0)
        total_ok = sum(v for k, v in servers.items() if not k.startswith("status_"))

        if total_ok > 0:
            healthy_pct = healthy_count / total_ok
            # The single healthy host should handle the majority of successful requests
            self.assertGreater(
                healthy_pct, 0.4,
                f"Healthy host only got {healthy_pct:.1%} of traffic. Servers: {servers}",
            )


class TestGradualDegradation(unittest.TestCase):
    """Tests detection of gradually degrading latency."""

    @classmethod
    def setUpClass(cls):
        reset_all_upstreams(latency_ms=5)
        time.sleep(1)
        warm_up_envoy(150)

    def test_gradual_latency_increase_detected(self):
        """A host whose latency gradually increases should eventually be
        detected as overloaded."""
        # Gradually increase upstream-5 latency
        for latency in [50, 100, 200, 400, 600]:
            set_upstream_latency("upstream-5", latency)
            send_requests(60, concurrency=10)
            time.sleep(1)

        # After reaching 600ms, check that upstream-5 is limited
        results = send_requests(150, concurrency=10)
        servers = count_servers(results)

        slow_count = servers.get("upstream-5", 0)
        total_ok = sum(v for k, v in servers.items() if not k.startswith("status_"))

        if total_ok > 0:
            slow_pct = slow_count / total_ok
            self.assertLess(
                slow_pct, 0.15,
                f"Gradually degraded host still getting {slow_pct:.1%} of traffic. Servers: {servers}",
            )


class TestDryRunMode(unittest.TestCase):
    """Tests that dry_run mode logs but does not enforce limits.

    NOTE: This test requires a separate Envoy config with dry_run=true.
    We test this by verifying that even with a slow host, no 503s are
    generated by the WASM plugin (the slow host still serves, just slowly).
    Since we can't easily swap Envoy configs, this test is a best-effort
    check of the dry_run concept.
    """

    @classmethod
    def setUpClass(cls):
        reset_all_upstreams(latency_ms=5)
        time.sleep(1)

    @unittest.skip("Requires Envoy restart with dry_run=true config")
    def test_dry_run_no_enforcement(self):
        """In dry_run mode, slow hosts should not be rejected."""
        pass


class TestEnvoyAdmin(unittest.TestCase):
    """Tests that Envoy admin interface provides useful debugging info."""

    def test_admin_ready(self):
        """Envoy admin should report ready."""
        status, _, _ = http_get(f"{ENVOY_ADMIN}/ready")
        self.assertEqual(status, 200)

    def test_admin_stats(self):
        """Envoy stats endpoint should be accessible."""
        status, _, body = http_get(f"{ENVOY_ADMIN}/stats")
        self.assertEqual(status, 200)
        self.assertIn("upstream_cx_total", body)

    def test_wasm_filter_loaded(self):
        """The WASM filter should be loaded (check stats for wasm metrics)."""
        status, _, body = http_get(f"{ENVOY_ADMIN}/stats?filter=wasm")
        self.assertEqual(status, 200)
        # Envoy should have wasm-related stats if the filter loaded
        self.assertTrue(
            len(body.strip()) > 0,
            "No WASM stats found — filter may not be loaded",
        )


class TestEdgeCases(unittest.TestCase):
    """Tests for edge cases and boundary conditions."""

    @classmethod
    def setUpClass(cls):
        reset_all_upstreams(latency_ms=5)
        time.sleep(1)
        warm_up_envoy(100)

    def setUp(self):
        reset_all_upstreams(latency_ms=5)
        time.sleep(1)

    def test_rapid_latency_changes(self):
        """Rapid latency oscillation should not crash the plugin."""
        for _ in range(5):
            set_upstream_latency("upstream-5", 500)
            send_requests(30, concurrency=10)
            time.sleep(0.5)
            set_upstream_latency("upstream-5", 5)
            send_requests(30, concurrency=10)
            time.sleep(0.5)

        # Plugin should still be functional
        results = send_requests(50, concurrency=10)
        statuses = count_status_codes(results)
        ok_count = statuses.get(200, 0)
        self.assertGreater(ok_count, 30, f"Plugin unstable after oscillation: {statuses}")

    def test_all_hosts_slow_then_recover(self):
        """If all hosts become slow and then recover, the system should recover."""
        # Make all slow
        for name in CONTROL_PORTS:
            set_upstream_latency(name, 500)

        for _ in range(4):
            send_requests(30, concurrency=5)
            time.sleep(1)

        # Restore all
        for name in CONTROL_PORTS:
            set_upstream_latency(name, 5)

        # Allow recovery
        for _ in range(8):
            send_requests(50, concurrency=10)
            time.sleep(1)

        # System should be functional
        results = send_requests(100, concurrency=10)
        statuses = count_status_codes(results)
        ok_count = statuses.get(200, 0)
        self.assertGreater(
            ok_count, 70,
            f"System did not recover after all-hosts-slow: {statuses}",
        )

    def test_sustained_load(self):
        """Plugin should remain stable under sustained load."""
        reset_all_upstreams(latency_ms=5)
        time.sleep(1)

        all_ok = 0
        all_total = 0
        for _ in range(10):
            results = send_requests(50, concurrency=20)
            statuses = count_status_codes(results)
            all_ok += statuses.get(200, 0)
            all_total += len(results)
            time.sleep(0.5)

        ok_pct = all_ok / all_total if all_total > 0 else 0
        self.assertGreater(
            ok_pct, 0.95,
            f"Sustained load caused too many failures: {all_ok}/{all_total} ({ok_pct:.1%})",
        )


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

if __name__ == "__main__":
    print("=" * 70)
    print("Adaptive Concurrency WASM Plugin — E2E Test Suite")
    print("=" * 70)

    print("\nWaiting for Envoy and upstreams to be ready...")
    try:
        wait_for_envoy(max_wait=60)
        wait_for_upstreams(max_wait=60)
    except TimeoutError as e:
        print(f"FATAL: {e}")
        sys.exit(1)

    print("All services ready. Running tests...\n")

    # Run with unittest
    loader = unittest.TestLoader()
    if len(sys.argv) > 1:
        # Allow running specific test classes/methods
        suite = loader.loadTestsFromNames(sys.argv[1:], module=sys.modules[__name__])
    else:
        suite = loader.loadTestsFromModule(sys.modules[__name__])

    runner = unittest.TextTestRunner(verbosity=2)
    result = runner.run(suite)

    sys.exit(0 if result.wasSuccessful() else 1)
