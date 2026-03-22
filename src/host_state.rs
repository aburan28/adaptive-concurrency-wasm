use std::collections::{HashMap, HashSet};

use crate::config::PluginConfig;
use crate::gradient2::{self, Gradient2Params, Gradient2Result};

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
            last_gradient: 1.0,
            total_requests: 0,
            total_limited: 0,
        }
    }

    pub fn record_request_start(&mut self) {
        self.in_flight = self.in_flight.saturating_add(1);
        self.total_requests += 1;
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
    /// Returns the result for logging purposes.
    pub fn recalculate_limit(&mut self, config: &PluginConfig) -> Option<Gradient2Result> {
        if self.latency_samples.is_empty() {
            return None;
        }

        self.latency_samples.sort_unstable();
        let sample_rtt = gradient2::percentile(&self.latency_samples, config.sample_percentile);

        // If we don't have a minRTT yet, use first window as baseline
        let min_rtt = match self.min_rtt_ns {
            Some(rtt) => rtt,
            None => {
                let baseline = gradient2::percentile(&self.latency_samples, config.sample_percentile);
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
            self.is_overloaded = true;
        } else if result.gradient > config.recovery_gradient_threshold {
            self.is_overloaded = false;
        }

        // Track minRTT recalculation schedule
        self.windows_since_min_rtt += 1;

        Some(result)
    }

    /// Start a minRTT probe window. Temporarily reduces traffic to measure baseline latency.
    pub fn start_min_rtt_probe(&mut self) {
        self.is_probing_min_rtt = true;
        self.probe_samples.clear();
    }

    /// Finish minRTT probe and update the baseline.
    pub fn finish_min_rtt_probe(&mut self, config: &PluginConfig) {
        if !self.probe_samples.is_empty() {
            self.probe_samples.sort_unstable();
            // Use the minimum observed latency during probe as the ideal RTT
            let probed_rtt = gradient2::percentile(&self.probe_samples, config.sample_percentile);
            // Only update if we got a valid measurement
            if probed_rtt > 0 {
                match self.min_rtt_ns {
                    Some(existing) => {
                        // Exponentially weighted: keep 80% old, 20% new
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
}

impl SharedState {
    pub fn new(config: PluginConfig) -> Self {
        Self {
            config,
            hosts: HashMap::new(),
            overloaded_hosts: HashSet::new(),
        }
    }

    pub fn get_or_create_host(&mut self, addr: &str, now_ns: u64) -> &mut HostState {
        let initial_limit = self.config.initial_concurrency_limit;
        self.hosts
            .entry(addr.to_string())
            .or_insert_with(|| HostState::new(initial_limit, now_ns))
    }

    /// Recalculate limits for all hosts with enough samples.
    /// Also manages minRTT probe lifecycle.
    pub fn recalculate_all_limits(&mut self) {
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
                if let Some(result) = host.recalculate_limit(&config) {
                    if old_limit != result.new_limit {
                        log::info!(
                            "adaptive_concurrency: host {} limit {} -> {} (gradient={:.3})",
                            addr,
                            old_limit,
                            result.new_limit,
                            result.gradient
                        );
                    }
                }

                // Check if it is time to start a minRTT probe
                if host.should_start_probe(config.min_rtt_recalc_windows) {
                    host.start_min_rtt_probe();
                    log::info!(
                        "adaptive_concurrency: host {} starting minRTT probe",
                        addr
                    );
                }
            }
        }
    }

    /// Rebuild the overloaded hosts set from current host states.
    pub fn update_overloaded_set(&mut self) {
        self.overloaded_hosts.clear();
        for (addr, host) in &self.hosts {
            if host.is_overloaded {
                self.overloaded_hosts.insert(addr.clone());
            }
        }
    }

    /// Remove hosts not seen within the expiry window.
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
}
