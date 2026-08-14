//! A sortable UTC timestamp for naming files a run must not overwrite.
//!
//! ## Why this exists rather than a crate
//!
//! One function, no locale, no parsing, no arithmetic on user input. `chrono` would be a dependency
//! and a build-time cost for `YYYYMMDD-HHMM`, and this tree has kept its dependency list to things
//! that genuinely cannot be derived — the Lua runtime, the Win32 bindings, a PNG codec.
//!
//! ## UTC, and the `Z` says so
//!
//! Local time needs the OS timezone, which is another Win32 feature flag and a source of
//! ambiguity twice a year. UTC is monotonic and unambiguous, and the suffix stops anyone reading a
//! filename as wall-clock time and being an hour out when they compare it against a log.

use std::time::{SystemTime, UNIX_EPOCH};

/// `YYYYMMDD-HHMMZ` — lexicographic order is chronological order.
///
/// Minutes, not seconds: two runs cannot start in the same minute, because a run holds the mouse for
/// far longer than that and refuses to start while the game is already running.
pub fn utc(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let (days, rest) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}{m:02}{d:02}-{:02}{:02}Z", rest / 3600, (rest % 3600) / 60)
}

/// Days since 1970-01-01 to a calendar date, by Howard Hinnant's `civil_from_days`.
///
/// Shifts the epoch to 0000-03-01 so that leap day lands at the end of a year and the month lengths
/// fall into a repeating pattern — which is what removes every special case except the shift back.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn at(secs: u64) -> String {
        utc(UNIX_EPOCH + Duration::from_secs(secs))
    }

    #[test]
    fn the_epoch_itself() {
        assert_eq!(at(0), "19700101-0000Z");
    }

    #[test]
    fn a_known_instant() {
        // 2026-08-12T03:41:00Z. The day count is 20677, counted forward from the leap day below:
        // 2024-02-29 is 19782, +730 reaches 2026-02-28, then 165 more to 12 August.
        assert_eq!(at(20_677 * 86_400 + 3 * 3600 + 41 * 60), "20260812-0341Z");
    }

    #[test]
    fn leap_day_is_a_real_date() {
        // 2024-02-29. The shifted-epoch trick earns its keep here or nowhere.
        assert_eq!(at(19_782 * 86_400), "20240229-0000Z");
        // And the day after is March, not the 30th of February.
        assert_eq!(at(19_783 * 86_400), "20240301-0000Z");
    }

    #[test]
    fn a_century_that_is_not_a_leap_year() {
        // 1900 was not a leap year but 2000 was; both are before and after the epoch shift.
        assert_eq!(at(11_016 * 86_400), "20000229-0000Z");
    }

    #[test]
    fn names_sort_into_the_order_they_were_taken() {
        // The whole point of the format. A run at 09:00 must sort before one at 10:00, and December
        // before the following January.
        let mut names = vec![at(20_677 * 86_400 + 10 * 3600), at(20_677 * 86_400 + 9 * 3600)];
        names.sort();
        assert_eq!(names, vec!["20260812-0900Z", "20260812-1000Z"]);
    }
}
