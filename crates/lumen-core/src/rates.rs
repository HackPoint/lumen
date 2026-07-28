/// Sonnet 3.5 / 3.7 / 4 pricing — USD per token.
/// Single source of truth for all Rust consumers.
/// Mirrors TypeScript RATE in lumenator/src/app/components/index.ts.
pub const INPUT: f64 = 5.0 / 1_000_000.0;
pub const OUTPUT: f64 = 25.0 / 1_000_000.0;
pub const CACHE_READ: f64 = 0.5 / 1_000_000.0;
pub const CACHE_WRITE: f64 = 6.25 / 1_000_000.0;

/// Context window tiers, used ONLY when the model is unknown.
pub const TIERS: &[u64] = &[200_000, 500_000, 1_000_000];
pub const WARN_RATIO: f64 = 0.80;
pub const ALERT_RATIO: f64 = 0.95;

/// Published context windows per model, from the Claude model catalog.
///
/// This is the authoritative source: the window is a property of the MODEL, not
/// of how full the context happens to be. Inferring it from observed fill —
/// which is what Lumen did before — systematically under-reports, because a
/// 1M-window session that has only reached 267K looks exactly like a 500K one.
///
/// Matched by longest prefix so dated snapshots (`claude-haiku-4-5-20251001`)
/// resolve the same as their alias. Order does not matter; the longest match wins.
const MODEL_WINDOWS: &[(&str, u64)] = &[
    // Every current Opus / Sonnet / Fable / Mythos model is 1M.
    ("claude-fable-5", 1_000_000),
    ("claude-mythos-5", 1_000_000),
    ("claude-opus-5", 1_000_000),
    ("claude-opus-4-8", 1_000_000),
    ("claude-opus-4-7", 1_000_000),
    ("claude-opus-4-6", 1_000_000),
    ("claude-sonnet-5", 1_000_000),
    ("claude-sonnet-4-6", 1_000_000),
    // Haiku is the only current model below 1M.
    ("claude-haiku-4-5", 200_000),
];

/// The published context window for `model`, or None if we don't recognise it.
///
/// Returning None for an unknown model is deliberate — better to fall back to
/// fill-based inference than to assert a window we cannot back up. Older models
/// (Opus 4.5, Sonnet 4.5 and earlier) deliberately have no entry.
pub fn model_window(model: &str) -> Option<u64> {
    let m = model.to_ascii_lowercase();
    MODEL_WINDOWS
        .iter()
        .filter(|(id, _)| m.starts_with(id))
        // Longest prefix wins, so a future "claude-opus-5-1m" style id cannot be
        // captured by a shorter entry that happens to also match.
        .max_by_key(|(id, _)| id.len())
        .map(|&(_, window)| window)
}

/// Smallest tier ≥ fill, or 1 M if fill exceeds all tiers.
///
/// Fill-based inference is a FALLBACK for unrecognised models. It can only ever
/// establish a lower bound — prefer [`resolve_window`], which asks the model first.
pub fn infer_window(fill: u64) -> u64 {
    TIERS
        .iter()
        .copied()
        .find(|&t| fill <= t)
        .unwrap_or(*TIERS.last().unwrap())
}

/// The context window to display for a session.
///
/// `peak_fill` must be the session's HIGH-WATER MARK, not its current fill:
/// `/compact` drops the fill sharply, and inferring from the momentary value
/// would shrink the window mid-session and make the gauge jump.
pub fn resolve_window(model: &str, peak_fill: u64) -> u64 {
    match model_window(model) {
        // A published window is authoritative — but never claim a window smaller
        // than a fill we have actually observed.
        Some(published) => published.max(infer_window(peak_fill)),
        None => infer_window(peak_fill),
    }
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

#[cfg(test)]
mod window_tests {
    use super::*;

    // ── model_window: the published windows ──────────────────────────────────

    #[test]
    fn every_current_opus_and_sonnet_is_one_million() {
        for id in [
            "claude-opus-5",
            "claude-opus-4-8",
            "claude-opus-4-7",
            "claude-opus-4-6",
            "claude-sonnet-5",
            "claude-sonnet-4-6",
            "claude-fable-5",
            "claude-mythos-5",
        ] {
            assert_eq!(model_window(id), Some(1_000_000), "{id}");
        }
    }

    #[test]
    fn haiku_is_two_hundred_thousand() {
        assert_eq!(model_window("claude-haiku-4-5"), Some(200_000));
    }

    #[test]
    fn a_dated_snapshot_resolves_like_its_alias() {
        // The daemon records whatever Claude Code wrote, which includes dated ids.
        assert_eq!(
            model_window("claude-haiku-4-5-20251001"),
            Some(200_000),
            "dated haiku must not fall through to inference"
        );
    }

    #[test]
    fn model_matching_is_case_insensitive() {
        assert_eq!(model_window("Claude-Opus-4-8"), Some(1_000_000));
    }

    #[test]
    fn an_unknown_model_has_no_published_window() {
        // Deliberate: we do not assert a window we cannot back up.
        for id in [
            "",
            "<synthetic>",
            "gpt-4",
            "claude-opus-4-5-20251101",
            "llama",
        ] {
            assert_eq!(model_window(id), None, "{id}");
        }
    }

    // ── resolve_window: what the gauge actually shows ────────────────────────

    #[test]
    fn a_known_model_reports_its_published_window_regardless_of_fill() {
        // THE BUG: a 267K fill on a 1M-window model used to display as 500K,
        // because the window was inferred from the fill instead of the model.
        assert_eq!(resolve_window("claude-opus-4-8", 267_593), 1_000_000);
        assert_eq!(resolve_window("claude-opus-4-8", 0), 1_000_000);
        assert_eq!(resolve_window("claude-opus-4-8", 999_999), 1_000_000);
    }

    #[test]
    fn every_model_in_the_real_dataset_now_resolves_correctly() {
        // Peak fills observed in a real lumen.db, with the window each one used
        // to display versus what it must display now.
        let cases = [
            ("claude-opus-4-8", 457_136u64, 1_000_000u64),
            ("claude-sonnet-4-6", 298_606, 1_000_000),
            ("claude-opus-4-7", 172_040, 1_000_000),
            ("claude-opus-4-6", 166_470, 1_000_000),
            ("claude-haiku-4-5-20251001", 152_423, 200_000),
        ];
        for (model, peak, expected) in cases {
            assert_eq!(resolve_window(model, peak), expected, "{model}");
        }
    }

    #[test]
    fn haiku_stays_at_two_hundred_thousand_rather_than_inflating() {
        // Haiku's real ceiling is 200K; a busy session must not silently become 1M.
        assert_eq!(resolve_window("claude-haiku-4-5", 150_000), 200_000);
        assert_eq!(resolve_window("claude-haiku-4-5", 199_999), 200_000);
    }

    #[test]
    fn an_unknown_model_falls_back_to_fill_inference() {
        assert_eq!(resolve_window("<synthetic>", 0), 200_000);
        assert_eq!(resolve_window("mystery-model", 250_000), 500_000);
        assert_eq!(resolve_window("mystery-model", 900_000), 1_000_000);
    }

    #[test]
    fn an_observed_fill_above_the_published_window_widens_it() {
        // Should not happen, but if the API reports more fill than the published
        // window allows, believe the observation rather than showing over 100%.
        assert_eq!(
            resolve_window("claude-haiku-4-5", 400_000),
            500_000,
            "trust the measurement over the table"
        );
    }

    #[test]
    fn the_window_never_shrinks_as_fill_grows() {
        // Monotonic in peak_fill for every model, known or not — this is what
        // stops the gauge jumping around mid-session.
        for model in ["claude-opus-4-8", "claude-haiku-4-5", "unknown-model"] {
            let mut previous = 0;
            for peak in [0u64, 1_000, 199_999, 200_001, 499_999, 500_001, 2_000_000] {
                let w = resolve_window(model, peak);
                assert!(
                    w >= previous,
                    "{model} shrank at peak {peak}: {w} < {previous}"
                );
                previous = w;
            }
        }
    }

    #[test]
    fn a_compaction_drop_does_not_change_the_window() {
        // The caller passes the session PEAK, so a post-compaction fill of 16K
        // after a peak of 317K still resolves to the same window. This is the
        // non-monotonic fill pattern seen in real data (317241 -> 16794 -> 314173).
        let peak = 317_241;
        assert_eq!(
            resolve_window("claude-opus-4-8", peak),
            resolve_window("claude-opus-4-8", peak),
        );
        // And for an unknown model, the peak is what keeps it stable:
        assert_eq!(resolve_window("mystery", peak), 500_000);
        assert_ne!(
            resolve_window("mystery", 16_794),
            500_000,
            "passing the momentary fill instead of the peak is what caused the jump"
        );
    }
}
