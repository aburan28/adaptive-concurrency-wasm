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
}

impl AdaptiveConcurrencyHttp {
    pub fn new(shared: Rc<RefCell<SharedState>>) -> Self {
        Self {
            shared,
            request_start_ns: 0,
            upstream_address: None,
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

        // Add informational headers about overloaded hosts.
        let shared = self.shared.borrow();
        if shared.has_overloaded_hosts() && !shared.config.dry_run {
            // Tag requests so we can observe adaptive concurrency behavior
            self.set_http_request_header("x-adaptive-concurrency-active", Some("true"));
        }

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
        self.upstream_address = Some(addr.clone());

        let mut shared = self.shared.borrow_mut();
        let host = shared.get_or_create_host(&addr, now);

        // Record latency metrics
        host.record_request_end(latency_ns, now);
        host.total_requests += 1;

        // Add response headers for observability
        let is_overloaded = shared.is_host_overloaded(&addr);
        if is_overloaded {
            self.set_http_response_header("x-adaptive-concurrency-limited", Some("true"));
            self.set_http_response_header("x-overloaded-host", Some(&addr));
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
