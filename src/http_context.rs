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
    tracked_in_flight: bool,
}

impl AdaptiveConcurrencyHttp {
    pub fn new(shared: Rc<RefCell<SharedState>>) -> Self {
        Self {
            shared,
            request_start_ns: 0,
            upstream_address: None,
            tracked_in_flight: false,
        }
    }

    fn now_ns(&self) -> u64 {
        self.get_current_time()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64
    }

    fn get_upstream_address(&self) -> Option<String> {
        // Try multiple property paths for upstream address
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

impl Context for AdaptiveConcurrencyHttp {
    fn on_done(&mut self) -> bool {
        // Final cleanup: ensure in_flight is decremented if we tracked it
        if self.tracked_in_flight {
            if let Some(ref addr) = self.upstream_address {
                let now = self.now_ns();
                let mut shared = self.shared.borrow_mut();
                if let Some(host) = shared.hosts.get_mut(addr) {
                    host.in_flight = host.in_flight.saturating_sub(1);
                    host.last_seen_ns = now;
                }
            }
            self.tracked_in_flight = false;
        }
        true
    }
}

impl HttpContext for AdaptiveConcurrencyHttp {
    fn on_http_request_headers(&mut self, _num_headers: usize, _end_of_stream: bool) -> Action {
        self.request_start_ns = self.now_ns();
        Action::Continue
    }

    fn on_http_response_headers(&mut self, _num_headers: usize, _end_of_stream: bool) -> Action {
        let now = self.now_ns();

        // Get the upstream host address
        let addr = match self.get_upstream_address() {
            Some(a) => a,
            None => return Action::Continue,
        };

        let latency_ns = now.saturating_sub(self.request_start_ns);

        let mut shared = self.shared.borrow_mut();

        // Record metrics for this host
        {
            let host = shared.get_or_create_host(&addr, now);
            host.record_request_start(); // Mark as in-flight
            host.record_request_end(latency_ns, now); // Record latency

            // Note: we increment and immediately "complete" because we learned
            // about the host on response. The in_flight counter here represents
            // a snapshot. We track the in_flight for the duration between
            // on_http_response_headers and on_done via the tracked_in_flight flag.
            host.record_request_start(); // Re-increment for the processing phase
        }
        self.tracked_in_flight = true;
        self.upstream_address = Some(addr.clone());

        // Check if this host is overloaded
        if shared.is_host_overloaded(&addr) {
            if let Some(host) = shared.hosts.get_mut(&addr) {
                host.total_limited += 1;
            }

            let dry_run = shared.config.dry_run;
            let response_code = shared.config.overload_response_code;

            if dry_run {
                log::warn!(
                    "adaptive_concurrency: [DRY RUN] would reject request to overloaded host {}",
                    addr
                );
                return Action::Continue;
            }

            // Drop the borrow before calling send_http_response
            drop(shared);

            log::warn!(
                "adaptive_concurrency: rejecting response from overloaded host {}, sending {}",
                addr,
                response_code
            );

            self.send_http_response(
                response_code,
                vec![
                    ("x-envoy-overloaded", "true"),
                    ("x-adaptive-concurrency-limited", "true"),
                    ("x-overloaded-host", &addr),
                ],
                Some(b"upstream host adaptive concurrency limit reached"),
            );
            return Action::Pause;
        }

        Action::Continue
    }

    fn on_log(&mut self) {
        // Decrement in_flight when the request fully completes
        if self.tracked_in_flight {
            if let Some(ref addr) = self.upstream_address {
                let now = self.now_ns();
                let mut shared = self.shared.borrow_mut();
                if let Some(host) = shared.hosts.get_mut(addr) {
                    host.in_flight = host.in_flight.saturating_sub(1);
                    host.last_seen_ns = now;
                }
            }
            self.tracked_in_flight = false;
        }
    }
}
