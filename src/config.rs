use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct PluginConfig {
    #[serde(default = "default_initial_limit")]
    pub initial_concurrency_limit: u32,

    #[serde(default = "default_min_limit")]
    pub min_concurrency_limit: u32,

    #[serde(default = "default_max_limit")]
    pub max_concurrency_limit: u32,

    #[serde(default = "default_sample_window_size")]
    pub sample_window_size: usize,

    #[serde(default = "default_min_rtt_recalc_windows")]
    pub min_rtt_recalc_windows: u32,

    #[serde(default = "default_min_rtt_buffer_pct")]
    pub min_rtt_buffer_pct: f64,

    #[serde(default = "default_sample_percentile")]
    pub sample_percentile: f64,

    #[serde(default = "default_max_gradient")]
    pub max_gradient: f64,

    #[serde(default = "default_tick_period_ms")]
    pub tick_period_ms: u64,

    #[serde(default = "default_host_expiry_secs")]
    pub host_expiry_secs: u64,

    /// HTTP status code to return when shedding load (default: 503)
    #[serde(default = "default_overload_response_code")]
    pub overload_response_code: u32,

    #[serde(default)]
    pub dry_run: bool,

    /// Gradient threshold below which a host is considered overloaded
    #[serde(default = "default_overload_gradient_threshold")]
    pub overload_gradient_threshold: f64,

    /// Gradient threshold above which an overloaded host is considered recovered
    #[serde(default = "default_recovery_gradient_threshold")]
    pub recovery_gradient_threshold: f64,

    /// Minimum number of probe samples before computing minRTT
    #[serde(default = "default_min_rtt_probe_count")]
    pub min_rtt_probe_count: usize,

    /// Seconds after which an overloaded host with no new samples gets reset
    #[serde(default = "default_recovery_timeout_secs")]
    pub recovery_timeout_secs: u64,
}

fn default_initial_limit() -> u32 {
    100
}
fn default_min_limit() -> u32 {
    3
}
fn default_max_limit() -> u32 {
    1000
}
fn default_sample_window_size() -> usize {
    250
}
fn default_min_rtt_recalc_windows() -> u32 {
    5
}
fn default_min_rtt_buffer_pct() -> f64 {
    0.25
}
fn default_sample_percentile() -> f64 {
    0.9
}
fn default_max_gradient() -> f64 {
    2.0
}
fn default_tick_period_ms() -> u64 {
    1000
}
fn default_host_expiry_secs() -> u64 {
    300
}
fn default_overload_response_code() -> u32 {
    503
}
fn default_overload_gradient_threshold() -> f64 {
    0.7
}
fn default_recovery_gradient_threshold() -> f64 {
    0.95
}
fn default_min_rtt_probe_count() -> usize {
    25
}
fn default_recovery_timeout_secs() -> u64 {
    10
}

impl Default for PluginConfig {
    fn default() -> Self {
        Self {
            initial_concurrency_limit: default_initial_limit(),
            min_concurrency_limit: default_min_limit(),
            max_concurrency_limit: default_max_limit(),
            sample_window_size: default_sample_window_size(),
            min_rtt_recalc_windows: default_min_rtt_recalc_windows(),
            min_rtt_buffer_pct: default_min_rtt_buffer_pct(),
            sample_percentile: default_sample_percentile(),
            max_gradient: default_max_gradient(),
            tick_period_ms: default_tick_period_ms(),
            host_expiry_secs: default_host_expiry_secs(),
            overload_response_code: default_overload_response_code(),
            dry_run: false,
            overload_gradient_threshold: default_overload_gradient_threshold(),
            recovery_gradient_threshold: default_recovery_gradient_threshold(),
            min_rtt_probe_count: default_min_rtt_probe_count(),
            recovery_timeout_secs: default_recovery_timeout_secs(),
        }
    }
}

impl PluginConfig {
    pub fn from_json(bytes: &[u8]) -> Result<Self, String> {
        serde_json::from_slice(bytes).map_err(|e| format!("failed to parse config: {}", e))
    }
}
