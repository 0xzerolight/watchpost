//! Figures derived from dense per-day series.
//!
//! Shared by the analytics page (portfolio totals, leaderboard columns) and
//! the repo page (KPI tiles). Everything here consumes the output of
//! [`crate::db::queries::dense_series`] and friends, and keeps that module's
//! discipline: `None` is a day nobody observed, never a zero.

use crate::routes::html::{PERIOD_COUNT, PERIODS};

/// Add one repo's dense series into the running portfolio total, in place.
///
/// A `None` contributes nothing rather than a zero: a day nobody observed is
/// unknown, and a repo watchpost had not started watching yet must not drag the
/// portfolio's total down to a level it never held. A repo's first observed day
/// is therefore a genuine step up in the total, which is the same thing
/// [`crate::db::queries::dense_downloads_total`] does when an asset first
/// appears.
pub fn add_into(total: &mut [Option<i64>], part: impl Iterator<Item = Option<i64>>) {
    for (slot, value) in total.iter_mut().zip(part) {
        if let Some(value) = value {
            *slot = Some(slot.unwrap_or(0) + value);
        }
    }
}

/// One figure per entry of [`PERIODS`], in that order, from the whole-history
/// series `values`.
///
/// The tail slices are the same ones `tail()` in assets/app.js takes to zoom a
/// chart, which is what keeps "last 30 days" in the table and the 30-day view of
/// the chart above it describing the same thirty days.
pub fn per_period(
    values: &[Option<i64>],
    figure: impl Fn(&[Option<i64>]) -> Option<i64>,
) -> [Option<i64>; PERIOD_COUNT] {
    PERIODS.map(|(days, _)| {
        let from = if days > 0 {
            values.len().saturating_sub(days as usize)
        } else {
            0
        };
        figure(&values[from..])
    })
}

/// How far a carried-forward level moved across the window: its last observed
/// value minus its first.
///
/// The first *observed* value, not the window's opening slot. A repo watchpost
/// started watching halfway through the window has no reading at the open, and
/// treating that gap as a zero would report the repo's entire star count as
/// growth — the fiction [`crate::db::queries::recent_changes`] refuses when it
/// drops a first observation. Anchoring on the first reading instead always
/// reports a real difference between two real readings; it is simply measured
/// over a shorter span than the column heading names, which is the honest
/// answer when a shorter span is all there is.
///
/// `None` when nothing in the window was observed at all — an empty cell, not a
/// confident zero.
pub fn growth(values: &[Option<i64>]) -> Option<i64> {
    let first = values.iter().find_map(|value| *value)?;
    let last = values.iter().rev().find_map(|value| *value)?;
    Some(last - first)
}

/// A rate series summed over the window, `None` only when nothing in it was
/// observed — the same distinction `agg`'s "sum" mode keeps client-side.
pub fn sum_observed(values: &[Option<i64>]) -> Option<i64> {
    values.iter().flatten().copied().reduce(|a, b| a + b)
}

/// The newest observed value — the level a carried-forward series stands at
/// today. `None` when the series was never observed at all.
pub fn last_observed(values: &[Option<i64>]) -> Option<i64> {
    values.iter().rev().find_map(|value| *value)
}

/// How a rate series' sum moved over each entry of [`PERIODS`] against the
/// same number of days just before it, as a whole percentage.
///
/// Complete UTC days only. The last slot is today, which GitHub is still
/// filling, so it is dropped before either window is cut; a half-counted day
/// compared with a whole one reads as a fall that never happened. `Some` only
/// when every day of both windows was observed and the earlier sum is above
/// zero: a gap is unknown traffic, and summing round it would also read as a
/// fall, while a zero base has no percentage at all. Averaging per observed
/// day was the rejected alternative — it compares two different sets of days
/// and hides that either window was incomplete. "All" has no earlier window and
/// is always `None`, as [`crate::routes::html::delta_badge`] leaves it blank.
pub fn per_period_vs_previous(values: &[Option<i64>]) -> [Option<i64>; PERIOD_COUNT] {
    let complete = &values[..values.len().saturating_sub(1)];
    PERIODS.map(|(days, _)| {
        let n = usize::try_from(days).ok().filter(|n| *n > 0)?;
        let split = complete.len().checked_sub(n)?;
        let start = split.checked_sub(n)?;
        // `Option`'s `Sum` is `None` as soon as one day is.
        let current: i64 = complete[split..].iter().copied().sum::<Option<i64>>()?;
        let previous: i64 = complete[start..split]
            .iter()
            .copied()
            .sum::<Option<i64>>()?;
        if previous <= 0 {
            return None;
        }
        Some(((current - previous) as f64 * 100.0 / previous as f64).round() as i64)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_into_treats_a_gap_as_unknown_not_zero() {
        let mut total = vec![None, Some(1), None];
        add_into(&mut total, [Some(5), None, None].into_iter());
        assert_eq!(total, vec![Some(5), Some(1), None]);
    }

    #[test]
    fn add_into_stops_at_the_shorter_of_the_two() {
        let mut total = vec![None, None];
        add_into(&mut total, [Some(1), Some(2), Some(3)].into_iter());
        assert_eq!(total, vec![Some(1), Some(2)]);
    }

    #[test]
    fn growth_anchors_on_the_first_reading_not_the_window_edge() {
        // One reading in the window: nothing is known to have moved, and the
        // repo's whole star count is not growth.
        assert_eq!(growth(&[None, None, Some(400)]), Some(0));
        assert_eq!(growth(&[None, Some(100), Some(140)]), Some(40));
        assert_eq!(growth(&[Some(100), Some(100), Some(140)]), Some(40));
    }

    #[test]
    fn growth_is_none_when_nothing_in_the_window_was_observed() {
        // An empty cell, not a confident zero.
        assert_eq!(growth(&[None, None]), None);
        assert_eq!(growth(&[]), None);
    }

    #[test]
    fn growth_reports_a_fall() {
        assert_eq!(growth(&[Some(140), Some(137)]), Some(-3));
    }

    #[test]
    fn sum_observed_is_none_only_when_nothing_was_observed() {
        assert_eq!(sum_observed(&[None, None]), None);
        // An observed zero is a number, not a gap.
        assert_eq!(sum_observed(&[None, Some(0)]), Some(0));
        assert_eq!(sum_observed(&[Some(3), None, Some(4)]), Some(7));
    }

    #[test]
    fn per_period_slices_the_same_tails_the_client_zooms_to() {
        let values: Vec<Option<i64>> = (0..400).map(|i| Some(i as i64)).collect();
        let figures = per_period(&values, |slice| Some(slice.len() as i64));
        for (i, (days, _)) in PERIODS.iter().enumerate() {
            let expected = if *days > 0 { (*days).min(400) } else { 400 };
            assert_eq!(figures[i], Some(expected), "period {days}");
        }
    }

    #[test]
    fn last_observed_skips_trailing_gaps() {
        assert_eq!(last_observed(&[Some(3), Some(5), None]), Some(5));
        assert_eq!(last_observed(&[None, None]), None);
        assert_eq!(last_observed(&[]), None);
    }

    #[test]
    fn per_period_does_not_overrun_a_short_series() {
        let values = vec![Some(1), Some(2)];
        let figures = per_period(&values, |slice| Some(slice.len() as i64));
        // The 7-day column over two days of history is two days, not a panic.
        assert_eq!(figures[0], Some(2));
    }

    /// Fourteen complete days and today's partial bucket.
    fn fortnight(previous: [Option<i64>; 7], current: [Option<i64>; 7]) -> Vec<Option<i64>> {
        let mut values = previous.to_vec();
        values.extend(current);
        values.push(Some(999));
        values
    }

    #[test]
    fn views_change_compares_complete_windows_and_drops_today() {
        // Previous week 70, current week 140; today's 999 is not counted.
        let out = per_period_vs_previous(&fortnight([Some(10); 7], [Some(20); 7]));
        assert_eq!(out[0], Some(100));
        // Thirty days need sixty complete days; fourteen are not enough.
        assert_eq!(out[1], None);
    }

    #[test]
    fn views_change_reports_a_fall_rounded_to_a_whole_percent() {
        // anki_miner_android, Sep 17-23 against Sep 24-30: 26 then 15.
        let previous = [4, 4, 4, 2, 1, 2, 9].map(Some);
        let current = [0, 2, 0, 2, 2, 2, 7].map(Some);
        assert_eq!(
            per_period_vs_previous(&fortnight(previous, current))[0],
            Some(-42)
        );
    }

    #[test]
    fn a_gap_in_either_window_gives_no_change() {
        // A missing day is unknown traffic, not a fall.
        let mut previous = [Some(10); 7];
        previous[3] = None;
        assert_eq!(
            per_period_vs_previous(&fortnight(previous, [Some(20); 7]))[0],
            None
        );
        let mut current = [Some(20); 7];
        current[6] = None;
        assert_eq!(
            per_period_vs_previous(&fortnight([Some(10); 7], current))[0],
            None
        );
        // Today's bucket may be a gap without hiding anything.
        let mut values = fortnight([Some(10); 7], [Some(20); 7]);
        *values.last_mut().unwrap() = None;
        assert_eq!(per_period_vs_previous(&values)[0], Some(100));
    }

    #[test]
    fn a_zero_base_gives_no_change() {
        // Nothing to divide by: no percentage, not "+Infinity%".
        let out = per_period_vs_previous(&fortnight([Some(0); 7], [Some(5); 7]));
        assert_eq!(out[0], None);
    }

    #[test]
    fn all_never_has_a_previous_window() {
        let values: Vec<Option<i64>> = vec![Some(1); 800];
        let out = per_period_vs_previous(&values);
        let all = PERIODS.iter().position(|(days, _)| *days < 0).unwrap();
        assert_eq!(out[all], None);
        // A year does: 730 complete days are there.
        let year = PERIODS.iter().position(|(days, _)| *days == 365).unwrap();
        assert_eq!(out[year], Some(0));
    }
}
