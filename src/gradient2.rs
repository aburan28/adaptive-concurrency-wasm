/// Parameters for the Gradient2 concurrency limit calculation.
pub struct Gradient2Params {
    pub current_limit: u32,
    pub min_rtt_ns: u64,
    pub sample_rtt_ns: u64,
    pub min_limit: u32,
    pub max_limit: u32,
    pub max_gradient: f64,
    pub min_rtt_buffer_pct: f64,
}

/// Result of a Gradient2 calculation, including the intermediate gradient value.
pub struct Gradient2Result {
    pub new_limit: u32,
    pub gradient: f64,
}

/// Calculate a new concurrency limit using the Gradient2 algorithm.
///
/// The algorithm computes:
///   buffered_min_rtt = min_rtt * (1 + buffer_pct)
///   gradient = clamp(buffered_min_rtt / sample_rtt, 0.5, max_gradient)
///   headroom = sqrt(current_limit)
///   new_limit = clamp(current_limit * gradient + headroom, min, max)
pub fn calculate_new_limit(params: &Gradient2Params) -> Gradient2Result {
    if params.min_rtt_ns == 0 || params.sample_rtt_ns == 0 {
        return Gradient2Result {
            new_limit: params.current_limit,
            gradient: 1.0,
        };
    }

    let buffered_min_rtt = params.min_rtt_ns as f64 * (1.0 + params.min_rtt_buffer_pct);
    let gradient = (buffered_min_rtt / params.sample_rtt_ns as f64).clamp(0.5, params.max_gradient);
    let headroom = (params.current_limit as f64).sqrt();
    let raw = params.current_limit as f64 * gradient + headroom;
    let new_limit = (raw.round() as u32).clamp(params.min_limit, params.max_limit);

    Gradient2Result {
        new_limit,
        gradient,
    }
}

/// Compute the value at a given percentile from a sorted slice of samples.
/// Uses nearest-rank method. `p` should be in [0.0, 1.0].
pub fn percentile(sorted_samples: &[u64], p: f64) -> u64 {
    if sorted_samples.is_empty() {
        return 0;
    }
    if sorted_samples.len() == 1 {
        return sorted_samples[0];
    }
    let idx = ((p * sorted_samples.len() as f64).ceil() as usize)
        .saturating_sub(1)
        .min(sorted_samples.len() - 1);
    sorted_samples[idx]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(current: u32, min_rtt: u64, sample_rtt: u64) -> Gradient2Params {
        Gradient2Params {
            current_limit: current,
            min_rtt_ns: min_rtt,
            sample_rtt_ns: sample_rtt,
            min_limit: 3,
            max_limit: 1000,
            max_gradient: 2.0,
            min_rtt_buffer_pct: 0.25,
        }
    }

    #[test]
    fn steady_state_grows_by_headroom() {
        // sample_rtt == buffered_min_rtt => gradient = 1.0
        // buffered = 5_000_000 * 1.25 = 6_250_000
        let r = calculate_new_limit(&params(100, 5_000_000, 6_250_000));
        assert!((r.gradient - 1.0).abs() < 0.001);
        // new = 100 * 1.0 + sqrt(100) = 110
        assert_eq!(r.new_limit, 110);
    }

    #[test]
    fn latency_increase_reduces_limit() {
        // sample_rtt = 12_500_000 (2x buffered) => gradient = 0.5
        let r = calculate_new_limit(&params(100, 5_000_000, 12_500_000));
        assert!((r.gradient - 0.5).abs() < 0.001);
        // new = 100 * 0.5 + 10 = 60
        assert_eq!(r.new_limit, 60);
    }

    #[test]
    fn gradient_floor_at_half() {
        // sample_rtt very high => gradient clamped to 0.5
        let r = calculate_new_limit(&params(100, 5_000_000, 100_000_000));
        assert!((r.gradient - 0.5).abs() < 0.001);
    }

    #[test]
    fn gradient_capped_at_max() {
        // sample_rtt very low => gradient clamped to max_gradient (2.0)
        let r = calculate_new_limit(&params(100, 5_000_000, 1_000_000));
        assert!((r.gradient - 2.0).abs() < 0.001);
        // new = 100 * 2.0 + 10 = 210
        assert_eq!(r.new_limit, 210);
    }

    #[test]
    fn limit_clamped_to_min() {
        let r = calculate_new_limit(&params(3, 5_000_000, 12_500_000));
        // new = 3 * 0.5 + sqrt(3) ≈ 3.2 => 3
        assert_eq!(r.new_limit, 3);
    }

    #[test]
    fn limit_clamped_to_max() {
        let mut p = params(900, 5_000_000, 1_000_000);
        p.max_limit = 1000;
        let r = calculate_new_limit(&p);
        // 900 * 2.0 + 30 = 1830 => clamped to 1000
        assert_eq!(r.new_limit, 1000);
    }

    #[test]
    fn zero_min_rtt_returns_current() {
        let r = calculate_new_limit(&params(100, 0, 5_000_000));
        assert_eq!(r.new_limit, 100);
    }

    #[test]
    fn zero_sample_rtt_returns_current() {
        let r = calculate_new_limit(&params(100, 5_000_000, 0));
        assert_eq!(r.new_limit, 100);
    }

    #[test]
    fn percentile_single_sample() {
        assert_eq!(percentile(&[42], 0.9), 42);
    }

    #[test]
    fn percentile_p90_ten_samples() {
        let samples: Vec<u64> = (1..=10).collect();
        // p90 of [1..10]: index = ceil(0.9 * 10) - 1 = 8 => value 9
        assert_eq!(percentile(&samples, 0.9), 9);
    }

    #[test]
    fn percentile_p50() {
        let samples: Vec<u64> = (1..=10).collect();
        // p50: index = ceil(0.5 * 10) - 1 = 4 => value 5
        assert_eq!(percentile(&samples, 0.5), 5);
    }

    #[test]
    fn percentile_p99() {
        let samples: Vec<u64> = (1..=100).collect();
        // p99: index = ceil(0.99 * 100) - 1 = 98 => value 99
        assert_eq!(percentile(&samples, 0.99), 99);
    }

    #[test]
    fn percentile_empty() {
        assert_eq!(percentile(&[], 0.9), 0);
    }

    #[test]
    fn convergence_under_sustained_latency() {
        // Simulate a host that goes from 5ms to 20ms sustained
        let mut limit = 100u32;
        let min_rtt = 5_000_000u64;
        let sample_rtt = 20_000_000u64; // 4x minRTT

        for _ in 0..20 {
            let r = calculate_new_limit(&Gradient2Params {
                current_limit: limit,
                min_rtt_ns: min_rtt,
                sample_rtt_ns: sample_rtt,
                min_limit: 3,
                max_limit: 1000,
                max_gradient: 2.0,
                min_rtt_buffer_pct: 0.25,
            });
            limit = r.new_limit;
        }
        // Should converge to a low value
        assert!(limit < 10, "limit should converge low, got {}", limit);
    }

    #[test]
    fn recovery_from_low_limit() {
        // Host recovers: sample_rtt goes back to buffered minRTT range
        let mut limit = 5u32;
        let min_rtt = 5_000_000u64;
        let sample_rtt = 6_500_000u64; // slightly above buffered

        for _ in 0..50 {
            let r = calculate_new_limit(&Gradient2Params {
                current_limit: limit,
                min_rtt_ns: min_rtt,
                sample_rtt_ns: sample_rtt,
                min_limit: 3,
                max_limit: 1000,
                max_gradient: 2.0,
                min_rtt_buffer_pct: 0.25,
            });
            limit = r.new_limit;
        }
        // Should recover significantly
        assert!(limit > 50, "limit should recover, got {}", limit);
    }
}
