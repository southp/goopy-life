//! Usage counters: how many instances were provisioned and how many failed to
//! (#172).
//!
//! Kept as a daily rollup plus an all-time total rather than derived from the
//! instance event log, which cannot answer the question: it records no
//! success, and its retention window would erase the all-time figure. Storage
//! grows with the number of days kept, not with the number of instances.
//!
//! Days are UTC calendar days.

use chrono::{Duration, NaiveDate};

/// The two things counted.
///
/// `Provisioned` is bumped when the spawn thread marks an instance `Done`,
/// which since #151 means it answered HTTP. `Failed` is bumped when a
/// spawn-phase failure marks it `Failed`. A failed despawn or sweep is a
/// cleanup problem, not a failed provision, and counts as neither.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageCounter {
    Provisioned,
    Failed,
}

/// A pair of counts over some span of days.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UsageCounts {
    pub provisioned: u64,
    pub failed: u64,
}

impl UsageCounts {
    fn add(&mut self, other: UsageCounts) {
        self.provisioned += other.provisioned;
        self.failed += other.failed;
    }
}

/// One UTC day's counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DailyUsage {
    pub day: NaiveDate,
    pub counts: UsageCounts,
}

/// Everything `GET /stats` reports, as of one UTC day.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageStats {
    /// Every provision since the counters were introduced. Never pruned.
    pub all_time: UsageCounts,
    /// The last [`UsageStats::WEEK_DAYS`] UTC days, today included.
    pub last_7_days: UsageCounts,
    /// Today so far.
    pub today: UsageCounts,
    /// Every day that has a row, newest first. Days with no activity have no
    /// row and are left out rather than zero-filled.
    pub daily: Vec<DailyUsage>,
}

impl UsageStats {
    /// How many days "weekly" spans, today included. Also the floor on the
    /// retention window: a shorter window would prune days the weekly figure
    /// still sums.
    pub const WEEK_DAYS: u32 = 7;

    /// Derive the summary figures from `daily` as seen on `today`.
    ///
    /// `daily` is sorted newest first here, whatever order it arrives in.
    /// Rows dated after `today` (a clock that stepped back) stay listed but
    /// are counted in neither `today` nor `last_7_days`.
    pub fn from_rows(all_time: UsageCounts, mut daily: Vec<DailyUsage>, today: NaiveDate) -> Self {
        daily.sort_by(|a, b| b.day.cmp(&a.day));

        let week_start = today - Duration::days(i64::from(Self::WEEK_DAYS) - 1);
        let mut last_7_days = UsageCounts::default();
        let mut today_counts = UsageCounts::default();
        for row in &daily {
            if row.day > today {
                continue;
            }
            if row.day >= week_start {
                last_7_days.add(row.counts);
            }
            if row.day == today {
                today_counts.add(row.counts);
            }
        }

        Self {
            all_time,
            last_7_days,
            today: today_counts,
            daily,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }

    fn row(d: &str, provisioned: u64, failed: u64) -> DailyUsage {
        DailyUsage {
            day: day(d),
            counts: UsageCounts {
                provisioned,
                failed,
            },
        }
    }

    #[test]
    fn the_week_is_seven_days_today_included() {
        let stats = UsageStats::from_rows(
            UsageCounts::default(),
            vec![
                row("2026-09-29", 1, 0),
                row("2026-09-23", 10, 1),  // six days back: inside
                row("2026-09-22", 100, 2), // seven days back: outside
            ],
            day("2026-09-29"),
        );

        assert_eq!(
            stats.last_7_days,
            UsageCounts {
                provisioned: 11,
                failed: 1
            }
        );
        assert_eq!(
            stats.today,
            UsageCounts {
                provisioned: 1,
                failed: 0
            }
        );
    }

    #[test]
    fn daily_is_newest_first_whatever_the_input_order() {
        let stats = UsageStats::from_rows(
            UsageCounts::default(),
            vec![row("2026-09-01", 1, 0), row("2026-09-29", 1, 0)],
            day("2026-09-29"),
        );

        let days: Vec<_> = stats.daily.iter().map(|r| r.day).collect();
        assert_eq!(days, [day("2026-09-29"), day("2026-09-01")]);
    }

    #[test]
    fn a_row_dated_after_today_is_listed_but_not_summed() {
        let stats = UsageStats::from_rows(
            UsageCounts::default(),
            vec![row("2026-09-30", 5, 5)],
            day("2026-09-29"),
        );

        assert_eq!(stats.today, UsageCounts::default());
        assert_eq!(stats.last_7_days, UsageCounts::default());
        assert_eq!(stats.daily.len(), 1);
    }

    #[test]
    fn no_rows_is_all_zeros() {
        let stats = UsageStats::from_rows(UsageCounts::default(), vec![], day("2026-09-29"));

        assert_eq!(stats.today, UsageCounts::default());
        assert_eq!(stats.last_7_days, UsageCounts::default());
        assert!(stats.daily.is_empty());
    }
}
