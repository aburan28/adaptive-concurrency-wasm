use proxy_wasm::hostcalls;
use proxy_wasm::types::MetricType;

/// Metric IDs for the adaptive concurrency plugin.
///
/// All metrics are prefixed with `adaptive_concurrency_` and appear in
/// Envoy's `/stats` endpoint. They are scrappable by Prometheus via
/// Envoy's stats sink or the `/stats/prometheus` admin endpoint.
///
/// Counters:
///   adaptive_concurrency_requests_total          — total requests observed
///   adaptive_concurrency_overloaded_responses     — responses from overloaded hosts
///   adaptive_concurrency_hosts_marked_overloaded  — times a host was marked overloaded
///   adaptive_concurrency_hosts_recovered          — times a host recovered (gradient-based)
///   adaptive_concurrency_recovery_timeouts        — times a host was force-recovered by timeout
///   adaptive_concurrency_limit_recalculations     — Gradient2 recalculations performed
///
/// Gauges:
///   adaptive_concurrency_tracked_hosts            — number of tracked upstream hosts
///   adaptive_concurrency_overloaded_hosts         — number of currently overloaded hosts
///   adaptive_concurrency_adaptive_timeout_ms      — current adaptive per-try timeout (ms)
///
/// Histograms:
///   adaptive_concurrency_request_latency_ms       — observed request latency (ms)
///   adaptive_concurrency_gradient                 — computed gradient values
pub struct PluginMetrics {
    // Counters
    pub requests_total: u32,
    pub requests_shed: u32,
    pub overloaded_responses: u32,
    pub hosts_marked_overloaded: u32,
    pub hosts_recovered: u32,
    pub recovery_timeouts: u32,
    pub limit_recalculations: u32,

    // Gauges
    pub tracked_hosts: u32,
    pub overloaded_hosts: u32,
    pub adaptive_timeout_ms: u32,

    // Histograms
    pub request_latency_ms: u32,
    pub gradient: u32,
}

impl PluginMetrics {
    /// Define all metrics with the Envoy host. Must be called once during
    /// `on_configure` or `on_vm_start`.
    pub fn new() -> Self {
        Self {
            // Counters
            requests_total: Self::define(
                MetricType::Counter,
                "adaptive_concurrency_requests_total",
            ),
            requests_shed: Self::define(MetricType::Counter, "adaptive_concurrency_requests_shed"),
            overloaded_responses: Self::define(
                MetricType::Counter,
                "adaptive_concurrency_overloaded_responses",
            ),
            hosts_marked_overloaded: Self::define(
                MetricType::Counter,
                "adaptive_concurrency_hosts_marked_overloaded",
            ),
            hosts_recovered: Self::define(
                MetricType::Counter,
                "adaptive_concurrency_hosts_recovered",
            ),
            recovery_timeouts: Self::define(
                MetricType::Counter,
                "adaptive_concurrency_recovery_timeouts",
            ),
            limit_recalculations: Self::define(
                MetricType::Counter,
                "adaptive_concurrency_limit_recalculations",
            ),

            // Gauges
            tracked_hosts: Self::define(MetricType::Gauge, "adaptive_concurrency_tracked_hosts"),
            overloaded_hosts: Self::define(
                MetricType::Gauge,
                "adaptive_concurrency_overloaded_hosts",
            ),
            adaptive_timeout_ms: Self::define(
                MetricType::Gauge,
                "adaptive_concurrency_adaptive_timeout_ms",
            ),

            // Histograms
            request_latency_ms: Self::define(
                MetricType::Histogram,
                "adaptive_concurrency_request_latency_ms",
            ),
            gradient: Self::define(MetricType::Histogram, "adaptive_concurrency_gradient"),
        }
    }

    fn define(metric_type: MetricType, name: &str) -> u32 {
        hostcalls::define_metric(metric_type, name).unwrap_or_else(|e| {
            log::error!(
                "adaptive_concurrency: failed to define metric '{}': {:?}",
                name,
                e
            );
            0
        })
    }

    // ---- Counter helpers ----

    pub fn inc_requests_total(&self) {
        hostcalls::increment_metric(self.requests_total, 1).ok();
    }

    pub fn inc_requests_shed(&self) {
        hostcalls::increment_metric(self.requests_shed, 1).ok();
    }

    pub fn inc_overloaded_responses(&self) {
        hostcalls::increment_metric(self.overloaded_responses, 1).ok();
    }

    pub fn inc_hosts_marked_overloaded(&self) {
        hostcalls::increment_metric(self.hosts_marked_overloaded, 1).ok();
    }

    pub fn inc_hosts_recovered(&self) {
        hostcalls::increment_metric(self.hosts_recovered, 1).ok();
    }

    pub fn inc_recovery_timeouts(&self) {
        hostcalls::increment_metric(self.recovery_timeouts, 1).ok();
    }

    pub fn inc_limit_recalculations(&self) {
        hostcalls::increment_metric(self.limit_recalculations, 1).ok();
    }

    // ---- Gauge helpers ----

    pub fn set_tracked_hosts(&self, value: u64) {
        hostcalls::record_metric(self.tracked_hosts, value).ok();
    }

    pub fn set_overloaded_hosts(&self, value: u64) {
        hostcalls::record_metric(self.overloaded_hosts, value).ok();
    }

    pub fn set_adaptive_timeout_ms(&self, value: u64) {
        hostcalls::record_metric(self.adaptive_timeout_ms, value).ok();
    }

    // ---- Histogram helpers ----

    pub fn record_request_latency_ms(&self, ms: u64) {
        hostcalls::record_metric(self.request_latency_ms, ms).ok();
    }

    pub fn record_gradient(&self, gradient: f64) {
        // Histograms accept u64; scale gradient by 1000 for 3 decimal places
        hostcalls::record_metric(self.gradient, (gradient * 1000.0) as u64).ok();
    }
}
