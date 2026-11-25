/*
 * When a market trades: Monday to Friday, inside its regular session (or the extended US day, 04:00–20:00). A part
 * of a bar window that holds no trading time can have no bars, so it is never asked of OpenD. Holidays are not
 * known here: a holiday is asked once, and then the store holds it as covered.
 */
use crate::market::Market;
use chrono::{Datelike, Duration, NaiveDate, TimeZone, Weekday};

/// The UNIX range [a, b] holds some trading time of `market`.
pub fn has_trading(market: Market, extended: bool, a: i64, b: i64) -> bool {
    if b < a {
        return false;
    }
    let tz = market.tz();
    let day = |t: i64| tz.timestamp_opt(t, 0).single().map(|d| d.date_naive());
    let (Some(first), Some(last)) = (day(a), day(b)) else { return false };
    let (opens, closes) = market.bounds(extended);
    let at = |d: NaiveDate, secs: u32| {
        let t = d.and_hms_opt(0, 0, 0)? + Duration::seconds(secs as i64);
        tz.from_local_datetime(&t).earliest().map(|x| x.timestamp())
    };
    let mut d = first;
    while d <= last {
        if !matches!(d.weekday(), Weekday::Sat | Weekday::Sun) {
            for (o, c) in opens.iter().zip(closes.iter()) {
                if let (Some(open), Some(close)) = (at(d, *o), at(d, *c)) {
                    if open <= b && close > a {
                        return true;
                    }
                }
            }
        }
        d = match d.succ_opt() {
            Some(n) => n,
            None => return false,
        };
    }
    false
}

/// Bars of `secs` may start in [a, b]: some bar start there holds trading time (daily bars start at midnight).
pub fn may_hold_bars(market: Market, extended: bool, secs: u32, a: i64, b: i64) -> bool {
    let a = if secs >= 86_400 { next_midnight(market, a) } else { a };
    a <= b && has_trading(market, extended, a, b + secs as i64 - 1)
}

/// The first local midnight of `market` at or after `t`.
pub fn next_midnight(market: Market, t: i64) -> i64 {
    let tz = market.tz();
    let Some(local) = tz.timestamp_opt(t, 0).single() else { return t };
    let midnight = |d: NaiveDate| d.and_hms_opt(0, 0, 0).and_then(|m| tz.from_local_datetime(&m).earliest()).map(|x| x.timestamp());
    match midnight(local.date_naive()) {
        Some(m) if m >= t => m,
        _ => local.date_naive().succ_opt().and_then(midnight).unwrap_or(t),
    }
}

/// `market` is trading at `t`.
pub fn is_open(market: Market, extended: bool, t: i64) -> bool {
    has_trading(market, extended, t, t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bars::unix_of;

    fn us(t: &str) -> i64 {
        unix_of(Market::Us, t).unwrap()
    }

    #[test]
    fn nights_and_weekends_hold_no_trading() {
        // Friday 2025-05-09 after the close until Monday before the open.
        assert!(!has_trading(Market::Us, false, us("2025-05-09 16:00:00"), us("2025-05-12 09:29:59")));
        assert!(has_trading(Market::Us, false, us("2025-05-09 16:00:00"), us("2025-05-12 09:30:00")));
        // The extended day reaches 20:00, and starts at 04:00.
        assert!(has_trading(Market::Us, true, us("2025-05-09 16:00:00"), us("2025-05-09 17:00:00")));
        assert!(!has_trading(Market::Us, true, us("2025-05-09 20:00:00"), us("2025-05-12 03:59:59")));
    }

    #[test]
    fn hong_kong_lunch_holds_no_trading() {
        let hk = |t: &str| unix_of(Market::Hk, t).unwrap();
        assert!(!has_trading(Market::Hk, false, hk("2025-05-09 12:00:00"), hk("2025-05-09 12:59:59")));
        assert!(is_open(Market::Hk, false, hk("2025-05-09 13:00:00")));
    }

    #[test]
    fn no_daily_bar_starts_between_friday_noon_and_saturday_noon() {
        assert!(!may_hold_bars(Market::Us, false, 86_400, us("2025-05-09 12:00:01"), us("2025-05-10 12:00:00")));
        assert!(may_hold_bars(Market::Us, false, 86_400, us("2025-05-08 12:00:01"), us("2025-05-10 12:00:00")));
        assert_eq!(next_midnight(Market::Us, us("2025-05-09 00:00:00")), us("2025-05-09 00:00:00"));
        assert_eq!(next_midnight(Market::Us, us("2025-05-09 00:00:01")), us("2025-05-10 00:00:00"));
    }

    #[test]
    fn a_saturday_is_closed() {
        assert!(!is_open(Market::Us, true, us("2025-05-10 12:00:00")));
        assert!(is_open(Market::Us, false, us("2025-05-09 12:00:00")));
    }
}
