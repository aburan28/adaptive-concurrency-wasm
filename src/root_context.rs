use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, UNIX_EPOCH};

use proxy_wasm::traits::{Context, HttpContext, RootContext};
use proxy_wasm::types::ContextType;

use crate::config::PluginConfig;
use crate::host_state::SharedState;
use crate::http_context::AdaptiveConcurrencyHttp;
use crate::metrics::PluginMetrics;
use crate::stats;

pub struct AdaptiveConcurrencyRoot {
    pub shared: Rc<RefCell<SharedState>>,
}

impl AdaptiveConcurrencyRoot {
    pub fn new() -> Self {
        Self {
            shared: Rc::new(RefCell::new(SharedState::new(PluginConfig::default()))),
        }
    }

    fn now_ns(&self) -> u64 {
        self.get_current_time()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64
    }
}

impl Context for AdaptiveConcurrencyRoot {}

impl RootContext for AdaptiveConcurrencyRoot {
    fn on_configure(&mut self, _plugin_configuration_size: usize) -> bool {
        let config = match self.get_plugin_configuration() {
            Some(bytes) => match PluginConfig::from_json(&bytes) {
                Ok(c) => {
                    log::info!(
                        "adaptive_concurrency: configured with initial_limit={}, min={}, max={}, window={}, dry_run={}",
                        c.initial_concurrency_limit,
                        c.min_concurrency_limit,
                        c.max_concurrency_limit,
                        c.sample_window_size,
                        c.dry_run,
                    );
                    c
                }
                Err(e) => {
                    log::error!("adaptive_concurrency: config parse error: {}", e);
                    return false;
                }
            },
            None => {
                log::info!("adaptive_concurrency: no config provided, using defaults");
                PluginConfig::default()
            }
        };

        let tick_ms = config.tick_period_ms;
        let mut shared = self.shared.borrow_mut();
        shared.config = config;
        shared.metrics = Some(PluginMetrics::new());
        drop(shared);
        self.set_tick_period(Duration::from_millis(tick_ms));
        true
    }

    fn on_tick(&mut self) {
        let now = self.now_ns();
        let mut shared = self.shared.borrow_mut();
        shared.recalculate_all_limits(now);
        shared.update_overloaded_set(now);
        shared.compute_adaptive_timeout();
        shared.expire_stale_hosts(now);
        stats::log_summary(&shared);
    }

    fn create_http_context(&self, _context_id: u32) -> Option<Box<dyn HttpContext>> {
        Some(Box::new(AdaptiveConcurrencyHttp::new(self.shared.clone())))
    }

    fn get_type(&self) -> Option<ContextType> {
        Some(ContextType::HttpContext)
    }
}
