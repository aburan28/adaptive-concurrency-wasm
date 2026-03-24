use std::collections::{HashMap, HashSet};

use crate::config::PluginConfig;
use crate::gradient2::{self, Gradient2Params, Gradient2Result};
use crate::metrics::PluginMetrics;

/// Per-host concurrency state tracked by the plugin.
pub struct HostState {
    pub current_limit: u32,
    pub in_flight: u32,
    pub latency_samples: Vec<u64>,
    pub min_rtt_ns: Option<u64>,
    pub windows_since_min_rtt: u32,
    pub is_probing_min_rtt: bool,
    pub probe_samples: Vec<u64>,
    pub last_seen_ns: u64,
    pub is_overloaded: bool,
    pub overloaded_since_ns: u64,
    pub last_gradient: f64,
    pub total_requests: u64,
    pub total_limited: u64,
}

impl HostState {
    pub fn new(initial_limit: u32, now_ns: u64) -> Self {
        Self {
            current_limit: initial_limit,
            in_flight: 0,
            latency_samples: Vec::with_capacity(256),
            min_rtt_ns: None,
            windows_since_min_rtt: 0,
            is_probing_min_rtt: false,
            probe_samples: Vec::new(),
            last_seen_ns: now_ns,
            is_overloaded: false,
            overloaded_since_ns: 0,
            last_gradient: 1.0,
            total_requests: 0,
            total_limited: 0,
        }
    }

    pub fn record_request_end(&mut self, latency_ns: u64, now_ns: u64) {
        self.in_flight = self.in_flight.saturating_sub(1);
        self.last_seen_ns = now_ns;

        if self.is_probing_min_rtt {
            self.probe_samples.push(latency_ns);
        } else {
            self.latency_samples.push(latency_ns);
        }
    }

    pub fn has_enough_samples(&self, window_size: usize) -> bool {
        self.latency_samples.len() >= window_size
    }

    pub fn has_enough_probe_samples(&self, count: usize) -> bool {
        self.probe_samples.len() >= count
    }

    /// Recalculate the concurrency limit using Gradient2.
    pub fn recalculate_limit(
        &mut self,
        config: &PluginConfig,
        now_ns: u64,
    ) -> Option<Gradient2Result> {
        if self.latency_samples.is_empty() {
            return None;
        }

        self.latency_samples.sort_unstable();
        let sample_rtt = gradient2::percentile(&self.latency_samples, config.sample_percentile);

        // If we don't have a minRTT yet, use first window as baseline
        let min_rtt = match self.min_rtt_ns {
            Some(rtt) => rtt,
            None => {
                let baseline =
                    gradient2::percentile(&self.latency_samples, config.sample_percentile);
                self.min_rtt_ns = Some(baseline);
                baseline
            }
        };

        let result = gradient2::calculate_new_limit(&Gradient2Params {
            current_limit: self.current_limit,
            min_rtt_ns: min_rtt,
            sample_rtt_ns: sample_rtt,
            min_limit: config.min_concurrency_limit,
            max_limit: config.max_concurrency_limit,
            max_gradient: config.max_gradient,
            min_rtt_buffer_pct: config.min_rtt_buffer_pct,
        });

        self.current_limit = result.new_limit;
        self.last_gradient = result.gradient;
        self.latency_samples.clear();

        // Update overload status based on gradient
        if result.gradient < config.overload_gradient_threshold {
            if !self.is_overloaded {
                self.overloaded_since_ns = now_ns;
            }
            self.is_overloaded = true;
        } else if result.gradient > config.recovery_gradient_threshold {
            self.is_overloaded = false;
        }

        self.windows_since_min_rtt += 1;

        Some(result)
    }

    pub fn start_min_rtt_probe(&mut self) {
        self.is_probing_min_rtt = true;
        self.probe_samples.clear();
    }

    pub fn finish_min_rtt_probe(&mut self, config: &PluginConfig) {
        if !self.probe_samples.is_empty() {
            self.probe_samples.sort_unstable();
            let probed_rtt = gradient2::percentile(&self.probe_samples, config.sample_percentile);
            if probed_rtt > 0 {
                match self.min_rtt_ns {
                    Some(existing) => {
                        self.min_rtt_ns =
                            Some(((existing as f64 * 0.8) + (probed_rtt as f64 * 0.2)) as u64);
                    }
                    None => {
                        self.min_rtt_ns = Some(probed_rtt);
                    }
                }
            }
        }
        self.is_probing_min_rtt = false;
        self.probe_samples.clear();
        self.windows_since_min_rtt = 0;
    }

    pub fn is_expired(&self, now_ns: u64, expiry_ns: u64) -> bool {
        now_ns.saturating_sub(self.last_seen_ns) > expiry_ns
    }

    pub fn should_start_probe(&self, recalc_windows: u32) -> bool {
        !self.is_probing_min_rtt && self.windows_since_min_rtt >= recalc_windows
    }
}

/// Shared state across all HTTP contexts, owned by RootContext.
pub struct SharedState {
    pub config: PluginConfig,
    pub hosts: HashMap<String, HostState>,
    pub overloaded_hosts: HashSet<String>,
    /// Adaptive per-try timeout in ms, computed from healthy host latencies.
    /// 0 means no override (use Envoy's default).
    pub adaptive_per_try_timeout_ms: u64,
    /// Fraction of incoming requests to shed (0.0 = none, 1.0 = all).
    /// Proportional to the fraction of overloaded hosts in the cluster.
    pub shed_fraction: f64,
    /// Simple counter used as a deterministic shed decision source.
    pub request_counter: u64,
    pub metrics: Option<PluginMetrics>,
}

impl SharedState {
    pub fn new(config: PluginConfig) -> Self {
        Self {
            config,
            hosts: HashMap::new(),
            overloaded_hosts: HashSet::new(),
            adaptive_per_try_timeout_ms: 0,
            shed_fraction: 0.0,
            request_counter: 0,
            metrics: None,
        }
    }

    pub fn get_or_create_host(&mut self, addr: &str, now_ns: u64) -> &mut HostState {
        let initial_limit = self.config.initial_concurrency_limit;
        self.hosts
            .entry(addr.to_string())
            .or_insert_with(|| HostState::new(initial_limit, now_ns))
    }

    /// Recalculate limits for all hosts with enough samples.
    pub fn recalculate_all_limits(&mut self, now_ns: u64) {
        let config = self.config.clone();
        for (addr, host) in self.hosts.iter_mut() {
            // Handle minRTT probe completion
            if host.is_probing_min_rtt && host.has_enough_probe_samples(config.min_rtt_probe_count)
            {
                host.finish_min_rtt_probe(&config);
                log::info!(
                    "adaptive_concurrency: host {} minRTT probe complete, minRTT={:?}us",
                    addr,
                    host.min_rtt_ns.map(|ns| ns / 1000)
                );
            }

            // Recalculate limit if we have enough samples
            if host.has_enough_samples(config.sample_window_size) {
                let old_limit = host.current_limit;
                let was_overloaded = host.is_overloaded;
                if let Some(result) = host.recalculate_limit(&config, now_ns) {
                    if let Some(ref m) = self.metrics {
                        m.inc_limit_recalculations();
                        m.record_gradient(result.gradient);
                    }
                    if old_limit != result.new_limit {
                        log::info!(
                            "adaptive_concurrency: host {} limit {} -> {} (gradient={:.3})",
                            addr,
                            old_limit,
                            result.new_limit,
                            result.gradient
                        );
                    }
                    // Detect state transitions
                    if host.is_overloaded && !was_overloaded {
                        if let Some(ref m) = self.metrics {
                            m.inc_hosts_marked_overloaded();
                        }
                    } else if !host.is_overloaded && was_overloaded {
                        if let Some(ref m) = self.metrics {
                            m.inc_hosts_recovered();
                        }
                    }
                }

                if host.should_start_probe(config.min_rtt_recalc_windows) {
                    host.start_min_rtt_probe();
                    log::info!("adaptive_concurrency: host {} starting minRTT probe", addr);
                }
            }
        }
    }

    /// Rebuild the overloaded hosts set. Also handles time-based recovery:
    /// if a host has been overloaded for longer than recovery_timeout without
    /// getting new samples, reset it to allow re-probing.
    pub fn update_overloaded_set(&mut self, now_ns: u64) {
        let recovery_ns = self.config.recovery_timeout_secs * 1_000_000_000;

        self.overloaded_hosts.clear();
        for (addr, host) in self.hosts.iter_mut() {
            if host.is_overloaded {
                // Time-based recovery: if overloaded too long, reset to allow probing
                let overloaded_duration = now_ns.saturating_sub(host.overloaded_since_ns);
                if recovery_ns > 0 && overloaded_duration > recovery_ns {
                    log::info!(
                        "adaptive_concurrency: host {} recovery timeout ({:.1}s), resetting overload status",
                        addr,
                        overloaded_duration as f64 / 1_000_000_000.0
                    );
                    host.is_overloaded = false;
                    // Reset minRTT so it gets re-measured with fresh data
                    host.min_rtt_ns = None;
                    host.current_limit = self.config.initial_concurrency_limit;
                    host.latency_samples.clear();
                    if let Some(ref m) = self.metrics {
                        m.inc_recovery_timeouts();
                    }
                } else {
                    self.overloaded_hosts.insert(addr.clone());
                }
            }
        }

        // Compute shed fraction: proportional to overloaded hosts in the cluster.
        // If 1 of 5 hosts is overloaded, shed ~20% of requests.
        let total = self.hosts.len();
        let overloaded = self.overloaded_hosts.len();
        self.shed_fraction = if total > 0 && overloaded > 0 {
            overloaded as f64 / total as f64
        } else {
            0.0
        };

        // Update gauges
        if let Some(ref m) = self.metrics {
            m.set_tracked_hosts(total as u64);
            m.set_overloaded_hosts(overloaded as u64);
        }
    }

    /// Compute an adaptive per-try timeout based on healthy host latencies.
    /// Uses recent latency samples from non-overloaded hosts to set a timeout
    /// that healthy hosts can comfortably meet but slow hosts will exceed.
    pub fn compute_adaptive_timeout(&mut self) {
        if self.overloaded_hosts.is_empty() {
            self.adaptive_per_try_timeout_ms = 0;
            return;
        }

        // Collect recent latency samples from healthy hosts
        let mut healthy_samples: Vec<u64> = Vec::new();
        for (addr, host) in self.hosts.iter() {
            if !host.is_overloaded && !self.overloaded_hosts.contains(addr) {
                healthy_samples.extend_from_slice(&host.latency_samples);
            }
        }

        if healthy_samples.is_empty() {
            // No recent samples; use a moderate fallback
            self.adaptive_per_try_timeout_ms = 200;
            return;
        }

        healthy_samples.sort_unstable();
        // Use p95 of healthy samples as baseline, then 3x multiplier
        let p95_idx = ((healthy_samples.len() as f64 * 0.95) as usize)
            .min(healthy_samples.len().saturating_sub(1));
        let healthy_p95_ns = healthy_samples[p95_idx];
        // Convert to ms with 3x safety margin, minimum 50ms, maximum 500ms
        let timeout_ms = (healthy_p95_ns / 1_000_000)
            .saturating_mul(3)
            .clamp(50, 500);
        self.adaptive_per_try_timeout_ms = timeout_ms;
        if let Some(ref m) = self.metrics {
            m.set_adaptive_timeout_ms(timeout_ms);
        }
        log::info!(
            "adaptive_concurrency: adaptive per-try timeout = {}ms (healthy p95={}ms, {} samples, overloaded={})",
            timeout_ms,
            healthy_p95_ns / 1_000_000,
            healthy_samples.len(),
            self.overloaded_hosts.len()
        );
    }

    pub fn expire_stale_hosts(&mut self, now_ns: u64) {
        let expiry_ns = self.config.host_expiry_secs * 1_000_000_000;
        self.hosts.retain(|addr, host| {
            let keep = !host.is_expired(now_ns, expiry_ns);
            if !keep {
                log::info!("adaptive_concurrency: expiring stale host {}", addr);
                self.overloaded_hosts.remove(addr);
            }
            keep
        });
    }

    pub fn is_host_overloaded(&self, addr: &str) -> bool {
        self.overloaded_hosts.contains(addr)
    }

    pub fn has_overloaded_hosts(&self) -> bool {
        !self.overloaded_hosts.is_empty()
    }

    /// Deterministic load shedding: returns true if this request should be
    /// rejected with 503. Uses a counter-based approach (not random) so that
    /// the shed rate converges exactly to `shed_fraction` over time.
    pub fn should_shed_request(&mut self) -> bool {
        if self.shed_fraction <= 0.0 {
            return false;
        }
        self.request_counter = self.request_counter.wrapping_add(1);
        // Shed every Nth request where N = 1/fraction.
        // E.g. fraction=0.2 → shed every 5th request.
        let period = (1.0 / self.shed_fraction).round() as u64;
        if period == 0 {
            return true; // fraction >= 1.0, shed everything
        }
        self.request_counter % period == 0
    }
}
