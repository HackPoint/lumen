//! Percentiles. Nearest-rank on the sorted values, so every figure is a value that was
//! actually observed and the result is an integer that is the same on every platform.

use serde::{Deserialize, Serialize};

/// The `p`-th percentile (1..=100) of `sorted` by the nearest-rank method: the value at
/// 1-based rank `ceil(p/100 * n)`. `None` for an empty slice.
pub fn percentile(sorted: &[i64], p: usize) -> Option<i64> {
    if sorted.is_empty() {
        return None;
    }
    let n = sorted.len();
    let rank = (p * n).div_ceil(100).clamp(1, n);
    Some(sorted[rank - 1])
}

/// Count, median and p90 of a set of whole numbers (tokens).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dist {
    pub n: usize,
    pub median: i64,
    pub p90: i64,
}

pub fn dist(values: &[i64]) -> Option<Dist> {
    let mut v = values.to_vec();
    v.sort_unstable();
    Some(Dist {
        n: v.len(),
        median: percentile(&v, 50)?,
        p90: percentile(&v, 90)?,
    })
}

/// Latency summary in nanoseconds: a monotonic clock's own unit, so a sub-microsecond round trip
/// is not rounded away before it is summarised.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lat {
    pub n: usize,
    pub min_ns: i64,
    pub median_ns: i64,
    pub p90_ns: i64,
    pub p99_ns: i64,
    pub max_ns: i64,
}

pub fn lat(values_ns: &[i64]) -> Option<Lat> {
    let mut v = values_ns.to_vec();
    v.sort_unstable();
    Some(Lat {
        n: v.len(),
        min_ns: *v.first()?,
        median_ns: percentile(&v, 50)?,
        p90_ns: percentile(&v, 90)?,
        p99_ns: percentile(&v, 99)?,
        max_ns: *v.last()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_rank_matches_the_textbook_definition() {
        let v: Vec<i64> = (1..=10).collect();
        assert_eq!(percentile(&v, 50), Some(5));
        assert_eq!(percentile(&v, 90), Some(9));
        assert_eq!(percentile(&v, 99), Some(10));
        assert_eq!(percentile(&v, 100), Some(10));
        assert_eq!(percentile(&[7], 50), Some(7));
        assert_eq!(percentile(&[], 50), None);
    }

    #[test]
    fn a_hundred_samples_put_p99_on_the_99th_value() {
        let v: Vec<i64> = (1..=100).collect();
        assert_eq!(percentile(&v, 99), Some(99));
        assert_eq!(percentile(&v, 90), Some(90));
        assert_eq!(percentile(&v, 50), Some(50));
    }

    #[test]
    fn dist_sorts_its_input() {
        let d = dist(&[9, 1, 5, 3, 7]).unwrap();
        assert_eq!((d.n, d.median, d.p90), (5, 5, 9));
    }
}
