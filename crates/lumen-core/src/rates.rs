/// Sonnet 3.5 / 3.7 / 4 pricing — USD per token.
/// Single source of truth for all Rust consumers.
/// Mirrors TypeScript RATE in lumenator/src/app/components/index.ts.
pub const INPUT: f64 = 5.0 / 1_000_000.0;
pub const OUTPUT: f64 = 25.0 / 1_000_000.0;
pub const CACHE_READ: f64 = 0.5 / 1_000_000.0;
pub const CACHE_WRITE: f64 = 6.25 / 1_000_000.0;

/// Context window tiers used for fill-ratio inference.
pub const TIERS: &[u64] = &[200_000, 500_000, 1_000_000];
pub const WARN_RATIO: f64 = 0.80;
pub const ALERT_RATIO: f64 = 0.95;

/// Smallest tier ≥ fill, or 1 M if fill exceeds all tiers.
pub fn infer_window(fill: u64) -> u64 {
    TIERS
        .iter()
        .copied()
        .find(|&t| fill <= t)
        .unwrap_or(*TIERS.last().unwrap())
}

pub fn session_cost(input: i64, output: i64, cache_read: i64, cache_write: i64) -> f64 {
    input as f64 * INPUT
        + output as f64 * OUTPUT
        + cache_read as f64 * CACHE_READ
        + cache_write as f64 * CACHE_WRITE
}

/// What Claude Code's prompt cache saved. Reported but not attributed to Lumen.
pub fn caching_savings(cache_read: i64) -> f64 {
    cache_read as f64 * (INPUT - CACHE_READ)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_cost_zero() {
        assert_eq!(session_cost(0, 0, 0, 0), 0.0);
    }

    #[test]
    fn session_cost_output_only() {
        // 1M output tokens @ $25/M = $25.00
        let cost = session_cost(0, 1_000_000, 0, 0);
        assert!((cost - 25.0).abs() < 1e-9, "got {cost}");
    }

    #[test]
    fn session_cost_input_only() {
        // 1M input tokens @ $5/M = $5.00
        let cost = session_cost(1_000_000, 0, 0, 0);
        assert!((cost - 5.0).abs() < 1e-9, "got {cost}");
    }

    #[test]
    fn session_cost_all_components() {
        // 1M input + 1M output + 1M cache_read + 1M cache_write
        // = 5.00 + 25.00 + 0.50 + 6.25 = 36.75
        let cost = session_cost(1_000_000, 1_000_000, 1_000_000, 1_000_000);
        assert!((cost - 36.75).abs() < 1e-9, "got {cost}");
    }

    #[test]
    fn caching_savings_zero() {
        assert_eq!(caching_savings(0), 0.0);
    }

    #[test]
    fn caching_savings_one_million() {
        // 1M cache_read * ($5/M − $0.50/M) = $4.50 saved
        let savings = caching_savings(1_000_000);
        assert!((savings - 4.5).abs() < 1e-9, "got {savings}");
    }

    #[test]
    fn infer_window_small_fits_first_tier() {
        assert_eq!(infer_window(0), 200_000);
        assert_eq!(infer_window(1), 200_000);
        assert_eq!(infer_window(199_999), 200_000);
        assert_eq!(infer_window(200_000), 200_000);
    }

    #[test]
    fn infer_window_mid_tier() {
        assert_eq!(infer_window(200_001), 500_000);
        assert_eq!(infer_window(500_000), 500_000);
    }

    #[test]
    fn infer_window_large_tier() {
        assert_eq!(infer_window(500_001), 1_000_000);
        assert_eq!(infer_window(1_000_000), 1_000_000);
    }

    #[test]
    fn infer_window_over_max_clamps_to_last_tier() {
        assert_eq!(infer_window(1_000_001), 1_000_000);
        assert_eq!(infer_window(u64::MAX / 2), 1_000_000);
    }
}
