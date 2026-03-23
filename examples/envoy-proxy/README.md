# Standalone Envoy Proxy — Adaptive Concurrency WASM Plugin

This directory contains configuration for running the adaptive concurrency
WASM plugin with standalone Envoy Proxy (no Kubernetes, no Envoy Gateway).

## Quick start

```bash
# 1. Build the WASM binary
make build

# 2. Run Envoy with Docker
docker run --rm -it \
  -v $(pwd)/examples/envoy-proxy/envoy.yaml:/etc/envoy/envoy.yaml:ro \
  -v $(pwd)/target/wasm32-unknown-unknown/release/adaptive_concurrency_wasm.wasm:/etc/envoy/plugin.wasm:ro \
  -p 10000:10000 -p 9901:9901 \
  envoyproxy/envoy:v1.31-latest \
  envoy -c /etc/envoy/envoy.yaml --log-level info --component-log-level wasm:debug

# 3. Or use the project's docker-compose
make docker-up
```

## Files

| File | Purpose |
|------|---------|
| `envoy.yaml` | Complete Envoy config with WASM filter, retries, and upstream cluster |
| `envoy-dry-run.yaml` | Same config but with `dry_run: true` (logging only, no enforcement) |

## How enforcement works

1. The WASM plugin tracks per-host latency using the Gradient2 algorithm
2. When a host's gradient drops below `overload_gradient_threshold` (0.7), it's marked overloaded
3. The plugin computes an adaptive per-try timeout from healthy host p95 latency
4. Envoy's retry policy retries timed-out requests to a different host
5. The `retry_host_predicate` ensures Envoy avoids retrying to the same host

## Key Envoy configuration points

### Retry policy (required for enforcement)

The retry policy is what makes enforcement effective. Without it, slow hosts
just respond slowly — the client still waits.

```yaml
retry_policy:
  retry_on: "5xx,reset,connect-failure,retriable-status-codes"
  num_retries: 3
  per_try_timeout: 0.15s          # Must be tight enough to catch slow hosts
  retriable_status_codes: [504]   # Timeout produces 504
  retry_host_predicate:           # Avoid retrying to the same host
    - name: envoy.retry_host_predicates.previous_hosts
      typed_config:
        "@type": type.googleapis.com/envoy.extensions.retry.host.previous_hosts.v3.PreviousHostsPredicate
  host_selection_retry_max_attempts: 5
```

### Tuning per_try_timeout

The `per_try_timeout` should be set to a value that:
- **Healthy hosts can meet**: e.g., if your healthy p99 is 50ms, set timeout to 150ms
- **Slow hosts exceed**: e.g., if degraded latency is 500ms+, 150ms timeout will catch it

The plugin also computes an adaptive timeout from healthy host samples, but the
Envoy route-level `per_try_timeout` acts as the primary enforcement mechanism.
