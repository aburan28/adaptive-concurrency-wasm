use std::cell::RefCell;
use std::rc::Rc;
use std::time::UNIX_EPOCH;

use proxy_wasm::traits::{Context, HttpContext};
use proxy_wasm::types::Action;

use crate::host_state::SharedState;

pub struct AdaptiveConcurrencyHttp {
    shared: Rc<RefCell<SharedState>>,
    request_start_ns: u64,
    upstream_address: Option<String>,
    was_shed: bool,
}

impl AdaptiveConcurrencyHttp {
    pub fn new(shared: Rc<RefCell<SharedState>>) -> Self {
        Self {
            shared,
            request_start_ns: 0,
            upstream_address: None,
            was_shed: false,
        }
    }

    fn now_ns(&self) -> u64 {
        self.get_current_time()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64
    }

    fn get_upstream_address(&self) -> Option<String> {
        self.get_property(vec!["upstream", "address"])
            .or_else(|| self.get_property(vec!["upstream", "uri"]))
            .and_then(|bytes| {
                let s = String::from_utf8(bytes).ok()?;
                if s.is_empty() {
                    None
                } else {
                    Some(s)
                }
            })
    }
}

impl Context for AdaptiveConcurrencyHttp {}

impl HttpContext for AdaptiveConcurrencyHttp {
    fn on_http_request_headers(&mut self, _num_headers: usize, _end_of_stream: bool) -> Action {
        self.request_start_ns = self.now_ns();

        let mut shared = self.shared.borrow_mut();

        if !shared.has_overloaded_hosts() || shared.config.dry_run {
            if shared.has_overloaded_hosts() {
                log::warn!(
                    "adaptive_concurrency: [DRY RUN] would shed {:.0}% of requests ({} overloaded hosts)",
                    shared.shed_fraction * 100.0,
                    shared.overloaded_hosts.len()
                );
            }
            return Action::Continue;
        }

        // Deterministic load shedding: reject a fraction of requests
        // proportional to the fraction of overloaded hosts.
        if shared.should_shed_request() {
            let response_code = shared.config.overload_response_code;
            let shed_pct = shared.shed_fraction * 100.0;
            let overloaded_count = shared.overloaded_hosts.len();

            if let Some(ref m) = shared.metrics {
                m.inc_requests_shed();
            }

            // Must drop the borrow before calling send_http_response
            drop(shared);

            self.was_shed = true;
            self.send_http_response(
                response_code,
                vec![
                    ("x-envoy-overloaded", "true"),
                    ("x-adaptive-concurrency-shed", "true"),
                    ("content-type", "text/plain"),
                ],
                Some(
                    format!(
                        "adaptive concurrency: request shed ({:.0}% of cluster overloaded, {} hosts)\n",
                        shed_pct, overloaded_count
                    )
                    .as_bytes(),
                ),
            );

            log::info!(
                "adaptive_concurrency: shed request (shed_fraction={:.2}, overloaded={})",
                shed_pct / 100.0,
                overloaded_count
            );

            return Action::Pause;
        }

        // Tag non-shed requests for observability
        self.set_http_request_header("x-adaptive-concurrency-active", Some("true"));
        Action::Continue
    }

    fn on_http_response_headers(&mut self, _num_headers: usize, _end_of_stream: bool) -> Action {
        // If this request was shed, we already sent a local reply; skip.
        if self.was_shed {
            return Action::Continue;
        }

        let now = self.now_ns();

        // Get the upstream host address
        let addr = match self.get_upstream_address() {
            Some(a) => a,
            None => return Action::Continue,
        };

        let latency_ns = now.saturating_sub(self.request_start_ns);
        let latency_ms = latency_ns / 1_000_000;
        self.upstream_address = Some(addr.clone());

        let mut shared = self.shared.borrow_mut();
        let host = shared.get_or_create_host(&addr, now);

        // Record latency — this is how the plugin learns about each host
        host.record_request_end(latency_ns, now);
        host.total_requests += 1;

        // Emit Envoy stats
        if let Some(ref m) = shared.metrics {
            m.inc_requests_total();
            m.record_request_latency_ms(latency_ms);
        }

        // Add response headers for observability
        let is_overloaded = shared.is_host_overloaded(&addr);
        if is_overloaded {
            self.set_http_response_header("x-adaptive-concurrency-limited", Some("true"));
            self.set_http_response_header("x-overloaded-host", Some(&addr));
            if let Some(ref m) = shared.metrics {
                m.inc_overloaded_responses();
            }
            if let Some(host) = shared.hosts.get_mut(&addr) {
                host.total_limited += 1;
            }
        }

        Action::Continue
    }

    fn on_log(&mut self) {
        // No-op. Metrics already recorded in on_http_response_headers.
    }
}
