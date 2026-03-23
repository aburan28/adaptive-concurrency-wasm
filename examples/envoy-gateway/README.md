# Envoy Gateway — Adaptive Concurrency WASM Plugin

This directory contains Kubernetes manifests for deploying the adaptive
concurrency WASM plugin with [Envoy Gateway](https://gateway.envoyproxy.io/).

## Prerequisites

- Kubernetes cluster with Envoy Gateway v1.4+ installed
- The WASM binary published as an OCI image **or** served over HTTP
- Gateway API CRDs installed (`gateway.networking.k8s.io`)

## Quick start

```bash
# 1. Install Envoy Gateway (if not already installed)
helm install eg oci://docker.io/envoyproxy/gateway-helm \
  --version v1.4.0 -n envoy-gateway-system --create-namespace

# 2. Apply the example manifests
kubectl apply -f namespace.yaml
kubectl apply -f gateway.yaml
kubectl apply -f backend.yaml
kubectl apply -f httproute.yaml
kubectl apply -f backend-traffic-policy.yaml
kubectl apply -f extension-policy.yaml

# 3. Verify
kubectl -n adaptive-concurrency get gateway,httproute,envoyextensionpolicy,backendtrafficpolicy
```

## Manifest overview

| File | Kind | Purpose |
|------|------|---------|
| `namespace.yaml` | Namespace | Dedicated namespace |
| `gateway.yaml` | GatewayClass + Gateway | Envoy Gateway listener on port 80 |
| `backend.yaml` | Deployment + Service | Example upstream service (5 replicas) |
| `httproute.yaml` | HTTPRoute | Routes `/` to the backend service |
| `backend-traffic-policy.yaml` | BackendTrafficPolicy | Retry + per-try timeout (critical for enforcement) |
| `extension-policy.yaml` | EnvoyExtensionPolicy | Loads the WASM plugin (OCI image) |
| `extension-policy-http.yaml` | EnvoyExtensionPolicy | Alternative: load WASM from HTTP URL |

## How the pieces fit together

```
Client
  │
  ▼
Gateway (port 80)
  │
  ├── HTTPRoute  ──►  backendRefs: my-app:8080
  │
  ├── BackendTrafficPolicy (retry + per-try timeout 150ms)
  │     • Slow hosts exceed 150ms → Envoy retries to a different host
  │     • retry_host_predicate avoids retrying to the same host
  │
  └── EnvoyExtensionPolicy (WASM filter)
        • Tracks per-host latency with Gradient2 algorithm
        • Marks hosts as overloaded when gradient < 0.7
        • Computes adaptive per-try timeout from healthy host latency
        • Adds observability headers (x-adaptive-concurrency-limited)
```

## Publishing the WASM binary as an OCI image

```bash
# Build the WASM binary
make build

# Push to a container registry using oras or buildah
oras push ghcr.io/yourorg/adaptive-concurrency-wasm:v0.1.0 \
  --artifact-type application/vnd.module.wasm.content.layer.v1+wasm \
  target/wasm32-unknown-unknown/release/adaptive_concurrency_wasm.wasm:application/wasm

# Or use Docker/buildah with a minimal Dockerfile:
# FROM scratch
# COPY target/wasm32-unknown-unknown/release/adaptive_concurrency_wasm.wasm /plugin.wasm
```

## Configuration reference

All fields are optional — sensible defaults are used if omitted.

| Parameter | Default | Description |
|-----------|---------|-------------|
| `initial_concurrency_limit` | 100 | Starting concurrency limit for new hosts |
| `min_concurrency_limit` | 3 | Floor — limit never drops below this |
| `max_concurrency_limit` | 1000 | Ceiling — limit never exceeds this |
| `sample_window_size` | 250 | Latency samples per recalculation window |
| `sample_percentile` | 0.9 | Percentile of latency samples used (p90) |
| `min_rtt_buffer_pct` | 0.25 | Buffer added to minRTT (25%) |
| `min_rtt_recalc_windows` | 5 | Windows between minRTT reprobes |
| `min_rtt_probe_count` | 25 | Samples needed during probe |
| `max_gradient` | 2.0 | Gradient cap (limits how fast limit grows) |
| `tick_period_ms` | 1000 | Recalculation interval |
| `host_expiry_secs` | 300 | Stale host timeout |
| `overload_gradient_threshold` | 0.7 | Gradient below this → host is overloaded |
| `recovery_gradient_threshold` | 0.95 | Gradient above this → host has recovered |
| `recovery_timeout_secs` | 10 | Force recovery after this many seconds |
| `dry_run` | false | Log-only mode (no enforcement) |
