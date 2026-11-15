/*
 * An exchange's trading week: its time zone and the sessions of a weekday. It tells which gaps of a series may
 * hold bars (so nights and weekends are never asked of a broker), the session text of `data.symbolInfo`, and where a
 * day starts (a daily bar starts at its day's midnight in the exchange's zone).
 */
use chrono::{Datelike, NaiveDate, TimeZone, Weekday};
use chrono_tz::Tz;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Calendar {
    pub tz: Tz,
    /// Monday to Friday, minutes from midnight: [open, close).
    pub sessions: &'static [(u32, u32)],
    /// `data.symbolInfo`'s session, such as `0915-1530`.
    pub session: &'static str,
}

const INDIA: Tz = chrono_tz::Asia::Kolkata;

impl Calendar {
    /// The Indian exchanges: NSE, BSE and their derivatives 09:15-15:30, MCX 09:00-23:30, currencies 09:00-17:00.
    pub fn india(exchange: &str) -> Calendar {
        match exchange {
            "MCX" | "NCO" => Calendar { tz: INDIA, sessions: &[(540, 1410)], session: "0900-2330" },
            "CDS" | "BCD" => Calendar { tz: INDIA, sessions: &[(540, 1020)], session: "0900-1700" },
            _ => Calendar { tz: INDIA, sessions: &[(555, 930)], session: "0915-1530" },
        }
    }

    pub fn timezone(&self) -> &'static str {
        self.tz.name()
    }

    /// The start of `t`'s day in the exchange's zone.
    pub fn midnight(&self, t: i64) -> i64 {
        let day = self.tz.timestamp_opt(t, 0).single().map(|d| d.date_naive()).unwrap_or_default();
        self.midnight_of(day)
    }

    fn midnight_of(&self, day: NaiveDate) -> i64 {
        self.tz.from_local_datetime(&day.and_hms_opt(0, 0, 0).unwrap_or_default()).earliest().map_or(0, |d| d.timestamp())
    }

    /// Whether a bar of `secs` (86400: daily) may start in [a, b]: a weekday's session (or its midnight, for daily
    /// bars) meets the range.
    pub fn may_hold_bars(&self, secs: u32, a: i64, b: i64) -> bool {
        if a > b {
            return false;
        }
        let first = self.tz.timestamp_opt(a, 0).single().map(|d| d.date_naive());
        let last = self.tz.timestamp_opt(b, 0).single().map(|d| d.date_naive());
        let (Some(mut day), Some(last)) = (first, last) else { return true };
        // Any week holds weekdays.
        if (last - day).num_days() > 7 {
            return true;
        }
        while day <= last {
            if !matches!(day.weekday(), Weekday::Sat | Weekday::Sun) {
                let midnight = self.midnight_of(day);
                if secs >= 86_400 {
                    if (a..=b).contains(&midnight) {
                        return true;
                    }
                } else {
                    for &(open, close) in self.sessions {
                        // The first bar may start before the open (its grid), the last starts before the close.
                        let (s, e) = (midnight + open as i64 * 60 - secs as i64 + 1, midnight + close as i64 * 60 - 1);
                        if s <= b && a <= e {
                            return true;
                        }
                    }
                }
            }
            day = match day.succ_opt() {
                Some(d) => d,
                None => return false,
            };
        }
        false
    }

    /// Whether `t` is in a weekday's session.
    pub fn is_open(&self, t: i64) -> bool {
        let Some(local) = self.tz.timestamp_opt(t, 0).single() else { return false };
        if matches!(local.weekday(), Weekday::Sat | Weekday::Sun) {
            return false;
        }
        let minute = ((t - self.midnight(t)) / 60) as u32;
        self.sessions.iter().any(|&(o, c)| (o..c).contains(&minute))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ist(s: &str) -> i64 {
        INDIA.from_local_datetime(&chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap()).unwrap().timestamp()
    }

    #[test]
    fn nights_and_weekends_hold_no_minutes() {
        let nse = Calendar::india("NSE");
        // Friday 15:30 to Monday 09:14.
        assert!(!nse.may_hold_bars(60, ist("2026-11-13 15:30"), ist("2026-11-16 09:14")));
        assert!(nse.may_hold_bars(60, ist("2026-11-13 15:00"), ist("2026-11-13 16:00")));
        assert!(nse.may_hold_bars(60, ist("2026-11-16 09:14"), ist("2026-11-16 09:15")));
        // MCX trades in the evening.
        assert!(Calendar::india("MCX").may_hold_bars(60, ist("2026-11-13 20:00"), ist("2026-11-13 21:00")));
    }

    #[test]
    fn a_daily_bar_starts_at_midnight_in_kolkata() {
        let nse = Calendar::india("NSE");
        assert_eq!(nse.midnight(ist("2026-11-13 10:00")), ist("2026-11-13 00:00"));
        // Saturday's and Sunday's midnights hold no daily bar.
        assert!(!nse.may_hold_bars(86_400, ist("2026-11-14 00:00"), ist("2026-11-15 23:59")));
        assert!(nse.may_hold_bars(86_400, ist("2026-11-14 00:00"), ist("2026-11-16 00:00")));
    }

    #[test]
    fn open_only_in_the_session() {
        let nse = Calendar::india("NSE");
        assert!(nse.is_open(ist("2026-11-13 09:15")));
        assert!(!nse.is_open(ist("2026-11-13 15:30")));
        assert!(!nse.is_open(ist("2026-11-14 10:00")));
    }
}
