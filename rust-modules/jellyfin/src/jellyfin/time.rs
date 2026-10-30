//! Jellyfin's time units into the common model's.
//!
//! Jellyfin measures durations and positions in .NET ticks (100 ns), and stamps dates as ISO-8601
//! (`2010-07-16T00:00:00.0000000Z`). The common model holds milliseconds, unix seconds and the
//! `YYYY-MM-DD` date Plex sends as `originallyAvailableAt`.

/// Ticks per millisecond.
pub const TICKS_PER_MS: i64 = 10_000;

/// Ticks into milliseconds, truncating; a missing or negative value is 0.
pub fn ticks_to_ms(ticks: Option<i64>) -> i64 {
    ticks.map_or(0, |t| (t / TICKS_PER_MS).max(0))
}

/// Milliseconds into ticks, for the requests that take a position (`/Sessions/Playing/Progress`).
pub fn ms_to_ticks(ms: i64) -> i64 {
    ms.max(0).saturating_mul(TICKS_PER_MS)
}

/// The `YYYY-MM-DD` date at the front of an ISO-8601 stamp, or `""` when it doesn't start with one.
pub fn iso_date(stamp: &str) -> String {
    let b = stamp.as_bytes();
    let shaped = b.len() >= 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b[..10].iter().enumerate().all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit());
    if shaped { stamp[..10].to_string() } else { String::new() }
}

/// An ISO-8601 UTC stamp (`YYYY-MM-DDTHH:MM:SS[.fffffff]Z`) into unix seconds, or 0 when it can't
/// be read. Jellyfin always sends UTC with a `Z`; an explicit offset is honoured as well.
pub fn iso_to_unix(stamp: &str) -> i64 {
    fn num(s: &str) -> Option<i64> {
        if s.is_empty() || !s.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        s.parse().ok()
    }
    let parse = || -> Option<i64> {
        if stamp.len() < 19 || iso_date(stamp).is_empty() {
            return None;
        }
        let (y, mo, d) = (num(stamp.get(0..4)?)?, num(stamp.get(5..7)?)?, num(stamp.get(8..10)?)?);
        let sep = stamp.as_bytes()[10];
        if sep != b'T' && sep != b' ' {
            return None;
        }
        let (h, mi, s) = (num(stamp.get(11..13)?)?, num(stamp.get(14..16)?)?, num(stamp.get(17..19)?)?);
        if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || s > 60 {
            return None;
        }
        // The zone: `Z`, nothing, or `+HH:MM` / `-HH:MM`, after any fractional seconds.
        let rest = stamp.get(19..)?.trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
        let offset = match rest.as_bytes().first() {
            None | Some(b'Z') | Some(b'z') => 0,
            Some(&sign @ (b'+' | b'-')) if rest.len() >= 6 => {
                let o = num(rest.get(1..3)?)? * 3600 + num(rest.get(4..6)?)? * 60;
                if sign == b'+' { o } else { -o }
            }
            _ => return None,
        };
        Some(days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + s - offset)
    };
    parse().unwrap_or(0)
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's `days_from_civil`).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticks_round_trip_through_milliseconds() {
        assert_eq!(ticks_to_ms(Some(72_000_000_000)), 7_200_000); // 2 h
        assert_eq!(ticks_to_ms(None), 0);
        assert_eq!(ticks_to_ms(Some(-5)), 0);
        assert_eq!(ms_to_ticks(7_200_000), 72_000_000_000);
        assert_eq!(ms_to_ticks(-1), 0);
    }

    #[test]
    fn iso_date_takes_the_calendar_date_only() {
        assert_eq!(iso_date("2010-07-16T00:00:00.0000000Z"), "2010-07-16");
        assert_eq!(iso_date("2010-07-16"), "2010-07-16");
        assert_eq!(iso_date("16/07/2010"), "");
        assert_eq!(iso_date(""), "");
    }

    #[test]
    fn iso_to_unix_reads_jellyfin_stamps() {
        assert_eq!(iso_to_unix("1970-01-01T00:00:00Z"), 0);
        assert_eq!(iso_to_unix("2026-10-13T22:14:32.1234567Z"), 1_791_929_672);
        assert_eq!(iso_to_unix("2000-02-29T12:00:00Z"), 951_825_600);
        assert_eq!(iso_to_unix("2026-10-13T23:14:32+01:00"), 1_791_929_672);
        assert_eq!(iso_to_unix("2026-10-13T22:14:32"), 1_791_929_672);
        assert_eq!(iso_to_unix("0001-01-01T00:00:00.0000000Z"), -62_135_596_800);
        assert_eq!(iso_to_unix("not a date"), 0);
        assert_eq!(iso_to_unix("2026-13-01T00:00:00Z"), 0);
        assert_eq!(iso_to_unix("2026-10-13T1\u{e9}:00:00Z"), 0, "a non-ASCII time is unreadable, not a panic");
        assert_eq!(iso_to_unix("2026-10-13T12:00:00+0\u{e9}:00"), 0);
    }
}
