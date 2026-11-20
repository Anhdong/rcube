//! Averages and records. All values are in centiseconds.

use crate::store::Solve;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Avg {
    Time(u64),
    Dnf,
}

/// "12.34" or "1:02.34".
pub fn fmt_cs(cs: u64) -> String {
    if cs >= 6000 {
        format!("{}:{:02}.{:02}", cs / 6000, (cs % 6000) / 100, cs % 100)
    } else {
        format!("{}.{:02}", cs / 100, cs % 100)
    }
}

pub fn fmt_avg(a: Option<Avg>) -> String {
    match a {
        Some(Avg::Time(cs)) => fmt_cs(cs),
        Some(Avg::Dnf) => "DNF".to_string(),
        None => "-".to_string(),
    }
}

/// How a single solve is shown: `12.34`, `14.34+` (+2) or `DNF`.
pub fn fmt_solve(s: &Solve) -> String {
    match s.effective_cs() {
        None => "DNF".to_string(),
        Some(cs) if s.penalty == crate::store::Penalty::Plus2 => format!("{}+", fmt_cs(cs)),
        Some(cs) => fmt_cs(cs),
    }
}

/// Average of `n` solves ending at index `end` (inclusive). The best and worst
/// 5% (at least one each) are dropped; DNFs count as the worst.
pub fn ao(solves: &[Solve], end: usize, n: usize) -> Option<Avg> {
    if n < 3 || end >= solves.len() || end + 1 < n {
        return None;
    }
    let mut v: Vec<u64> = solves[end + 1 - n..=end]
        .iter()
        .map(|s| s.effective_cs().unwrap_or(u64::MAX))
        .collect();
    v.sort_unstable();
    let trim = n.div_ceil(20);
    let mid = &v[trim..n - trim];
    if mid.contains(&u64::MAX) {
        return Some(Avg::Dnf);
    }
    let sum: u64 = mid.iter().sum();
    let cnt = mid.len() as u64;
    Some(Avg::Time((2 * sum + cnt) / (2 * cnt))) // round to nearest
}

/// Mean of all non-DNF solves.
pub fn mean(solves: &[Solve]) -> Option<u64> {
    let v: Vec<u64> = solves.iter().filter_map(|s| s.effective_cs()).collect();
    if v.is_empty() {
        None
    } else {
        Some(v.iter().sum::<u64>() / v.len() as u64)
    }
}

/// Cached per-solve data, recomputed whenever the list changes.
#[derive(Default)]
pub struct Stats {
    /// ao5 ending at each solve.
    pub ao5: Vec<Option<Avg>>,
    /// Index of the best single (first one wins ties, so equalling a record
    /// is not "a new best").
    pub best_single: Option<usize>,
    /// Index of the solve that ends the best ao5.
    pub best_ao5: Option<usize>,
}

impl Stats {
    pub fn compute(solves: &[Solve]) -> Self {
        let ao5: Vec<Option<Avg>> = (0..solves.len()).map(|i| ao(solves, i, 5)).collect();

        let mut best_single: Option<(usize, u64)> = None;
        for (i, s) in solves.iter().enumerate() {
            if let Some(cs) = s.effective_cs() {
                if best_single.map_or(true, |(_, b)| cs < b) {
                    best_single = Some((i, cs));
                }
            }
        }

        let mut best_ao5: Option<(usize, u64)> = None;
        for (i, a) in ao5.iter().enumerate() {
            if let Some(Avg::Time(cs)) = a {
                if best_ao5.map_or(true, |(_, b)| *cs < b) {
                    best_ao5 = Some((i, *cs));
                }
            }
        }

        Stats {
            ao5,
            best_single: best_single.map(|(i, _)| i),
            best_ao5: best_ao5.map(|(i, _)| i),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Penalty;

    fn mk(ms: u64, p: Penalty) -> Solve {
        Solve::new(ms, p, String::new())
    }

    fn times(ms: &[u64]) -> Vec<Solve> {
        ms.iter().map(|&m| mk(m, Penalty::None)).collect()
    }

    #[test]
    fn formatting() {
        assert_eq!(fmt_cs(987), "9.87");
        assert_eq!(fmt_cs(5), "0.05");
        assert_eq!(fmt_cs(6205), "1:02.05");
    }

    #[test]
    fn ao5_drops_best_and_worst() {
        let s = times(&[10_000, 11_000, 12_000, 9_000, 13_000]);
        assert_eq!(ao(&s, 4, 5), Some(Avg::Time(1100)));
        assert_eq!(ao(&s, 3, 5), None);
    }

    #[test]
    fn one_dnf_is_dropped_two_make_dnf() {
        let mut s = times(&[10_000, 11_000, 12_000, 9_000, 13_000]);
        s[4].penalty = Penalty::Dnf;
        // sorted: 9, 10, 11, 12, DNF -> mean(10,11,12) = 11
        assert_eq!(ao(&s, 4, 5), Some(Avg::Time(1100)));
        s[0].penalty = Penalty::Dnf;
        assert_eq!(ao(&s, 4, 5), Some(Avg::Dnf));
    }

    #[test]
    fn plus_two_counts() {
        let mut s = times(&[10_000, 10_000, 10_000, 10_000, 10_000]);
        s[2].penalty = Penalty::Plus2; // 12.00
        // sorted: 10,10,10,10,12 -> drop 10 and 12 -> mean(10,10,10)
        assert_eq!(ao(&s, 4, 5), Some(Avg::Time(1000)));
    }

    #[test]
    fn records_use_first_occurrence() {
        let s = times(&[10_000, 9_000, 9_000, 11_000, 12_000, 8_000]);
        let st = Stats::compute(&s);
        assert_eq!(st.best_single, Some(5));
        let s = times(&[10_000, 9_000, 9_000]);
        assert_eq!(Stats::compute(&s).best_single, Some(1));
    }

    #[test]
    fn best_ao5_tracked() {
        let s = times(&[10_000, 10_000, 10_000, 10_000, 10_000, 5_000, 5_000]);
        let st = Stats::compute(&s);
        assert_eq!(st.best_ao5, Some(6));
        assert_eq!(st.ao5[3], None);
    }
}
