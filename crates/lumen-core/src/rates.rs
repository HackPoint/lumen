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
