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
        // Per-host rejection happens in on_http_response_headers once we
        // know which upstream host was selected by the load balancer.
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
        let latency_ms = latency_ns / 1_000_000;
        self.upstream_address = Some(addr.clone());

        let mut shared = self.shared.borrow_mut();
        let host = shared.get_or_create_host(&addr, now);

        // Record latency — this is how the plugin learns about each host.
        // Always recorded so overloaded hosts can be detected and recovered.
        host.record_request_end(latency_ns, now);
        host.total_requests += 1;

        // Emit Envoy stats
        if let Some(ref m) = shared.metrics {
            m.inc_requests_total();
            m.record_request_latency_ms(latency_ms);
        }

        // Per-host rejection: if THIS host is overloaded, send a local 503
        // so Envoy retries to a different host (via retry_host_predicate:
        // previous_hosts). Requests to healthy hosts are never affected.
        let is_overloaded = shared.is_host_overloaded(&addr);
        if is_overloaded && !shared.config.dry_run {
            if let Some(ref m) = shared.metrics {
                m.inc_overloaded_responses();
                m.inc_requests_shed();
            }
            if let Some(host) = shared.hosts.get_mut(&addr) {
                host.total_limited += 1;
            }

            let response_code = shared.config.overload_response_code;
            drop(shared);

            log::info!(
                "adaptive_concurrency: rejecting response from overloaded host {} (latency={}ms)",
                addr,
                latency_ms
            );

            self.send_http_response(
                response_code,
                vec![
                    ("x-envoy-overloaded", "true"),
                    ("x-adaptive-concurrency-shed", "true"),
                    ("x-overloaded-host", &addr),
                    ("content-type", "text/plain"),
                ],
                Some(
                    format!(
                        "adaptive concurrency: host {} overloaded, retrying on another host\n",
                        addr
                    )
                    .as_bytes(),
                ),
            );
            return Action::Pause;
        }

        if is_overloaded {
            // Dry-run mode: annotate but don't reject
            log::warn!(
                "adaptive_concurrency: [DRY RUN] would reject response from overloaded host {} (latency={}ms)",
                addr,
                latency_ms
            );
            self.set_http_response_header("x-adaptive-concurrency-limited", Some("true"));
            self.set_http_response_header("x-overloaded-host", Some(&addr));
        }

        Action::Continue
    }

    fn on_log(&mut self) {
        // No-op. Metrics already recorded in on_http_response_headers.
    }
}
