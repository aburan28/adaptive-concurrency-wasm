use crate::host_state::SharedState;

/// Log a summary of all tracked hosts.
pub fn log_summary(shared: &SharedState) {
    let total_hosts = shared.hosts.len();
    let overloaded = shared.overloaded_hosts.len();
    let total_in_flight: u32 = shared.hosts.values().map(|h| h.in_flight).sum();

    if total_hosts > 0 {
        log::info!(
            "adaptive_concurrency: tracking {} hosts, {} overloaded, {} total in-flight",
            total_hosts,
            overloaded,
            total_in_flight
        );
    }

    // Log details for overloaded hosts
    for addr in &shared.overloaded_hosts {
        if let Some(host) = shared.hosts.get(addr) {
            log::warn!(
                "adaptive_concurrency: OVERLOADED host={} limit={} in_flight={} gradient={:.3} minRTT={}us total_limited={}",
                addr,
                host.current_limit,
                host.in_flight,
                host.last_gradient,
                host.min_rtt_ns.unwrap_or(0) / 1000,
                host.total_limited,
            );
        }
    }
}
