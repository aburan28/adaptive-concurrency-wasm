# Adaptive Concurrency WASM Plugin for Envoy

A per-host adaptive concurrency limiter for Envoy, implemented as a Rust WASM filter using the Gradient2 algorithm.

## Problem

Envoy's built-in adaptive concurrency filter operates per-cluster. When 1 out of 100 hosts degrades, the entire cluster gets throttled. This plugin tracks concurrency limits **per upstream host**, so only the degraded host is affected while the other 99 continue at full throughput.

## How It Works

1. **Per-host tracking**: The plugin maintains independent latency statistics and concurrency limits for each upstream host
2. **Gradient2 algorithm**: Computes an optimal concurrency limit based on the ratio of ideal latency (minRTT) to observed latency (sampleRTT)
3. **Enforcement via retry**: When a host is detected as overloaded, the plugin sends a 503 local reply which triggers Envoy's retry policy to route to a different host
4. **Automatic recovery**: As host latency improves, the algorithm gradually increases the concurrency limit

### Gradient2 Formula

```
buffered_min_rtt = min_rtt * (1 + buffer_pct)
gradient = clamp(buffered_min_rtt / sample_rtt, 0.5, max_gradient)
headroom = sqrt(current_limit)
new_limit = clamp(current_limit * gradient + headroom, min, max)
```

## Prerequisites

- Rust toolchain with `wasm32-unknown-unknown` target
- Docker and Docker Compose (for testing)

## Build

```bash
# Install wasm target if needed
rustup target add wasm32-unknown-unknown

# Build the WASM binary
make build

# Run unit tests
make test-auto
```

## Configuration

The plugin is configured via JSON in the Envoy WASM filter config:

| Field | Default | Description |
|---|---|---|
| `initial_concurrency_limit` | 100 | Starting limit per new host |
| `min_concurrency_limit` | 3 | Minimum limit floor |
| `max_concurrency_limit` | 1000 | Maximum limit ceiling |
| `sample_window_size` | 250 | Latency samples before recalculation |
| `min_rtt_recalc_windows` | 5 | Windows between minRTT reprobes |
| `min_rtt_buffer_pct` | 0.25 | Buffer added to minRTT (25%) |
| `sample_percentile` | 0.9 | Percentile for sampleRTT (p90) |
| `max_gradient` | 2.0 | Upper bound on gradient |
| `tick_period_ms` | 1000 | Periodic recalculation interval |
| `host_expiry_secs` | 300 | Stale host cleanup timeout |
| `overload_response_code` | 503 | HTTP status for overloaded hosts |
| `overload_gradient_threshold` | 0.7 | Gradient below which host is overloaded |
| `recovery_gradient_threshold` | 0.95 | Gradient above which host recovers |
| `dry_run` | false | Log-only mode (no enforcement) |

## Envoy Requirements

The WASM filter **must** be paired with a retry policy:

```yaml
retry_policy:
  retry_on: "5xx"
  num_retries: 3
  retry_host_predicate:
    - name: envoy.retry_host_predicates.previous_hosts
  host_selection_retry_max_attempts: 5
```

Without this, the 503 local replies will be returned directly to clients.

## Testing

```bash
# Start Envoy + 5 upstream servers (one is slow)
make docker-up

# Watch Envoy logs for adaptive concurrency messages
make docker-logs

# Send test traffic
make load-test

# Tear down
make docker-down
```

The test environment starts 5 upstream servers: 4 with 5ms latency and 1 with 500ms latency. The plugin should detect the slow host and start rejecting + retrying requests away from it.

## Architecture

```
Request -> Envoy -> WASM Filter -> Upstream Host
                      |
                      |-- on_http_request_headers: record start time
                      |-- on_http_response_headers: learn upstream, record latency,
                      |                             check overload -> 503 if overloaded
                      |-- on_log: cleanup in-flight tracking
                      |-- on_tick (periodic): recalculate Gradient2 limits,
                                              update overloaded set, expire stale hosts
```

State is shared between the RootContext (periodic processing) and HttpContext instances (per-request) via `Rc<RefCell<SharedState>>`.
