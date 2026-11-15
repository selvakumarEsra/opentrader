/*
 * History bars: the app's resolution as an OpenD K-line type, the paged request, and OpenD's K-lines as the app's
 * bars — `[time, open, high, low, close, volume]`, `time` the bar's START in UNIX seconds.
 *
 * OpenD writes a K-line's time as local wall time of the market ("2025-05-09 09:31:00"). Whether an intraday time
 * is the bar's start or its end is read from the bars themselves: end-stamped bars put one on a session's close
 * (16:00) and none on its open (09:30). Those are moved back by one interval.
 */
use crate::market::Market;
use crate::opend::client::Client;
use crate::opend::proto::{self, KLine, Security};
use crate::pace::Pace;
use chrono::{NaiveDateTime, TimeZone, Timelike};
use serde_json::{json, Value};

/// Bars one answer may carry; more come in later pages.
const PAGE: u32 = 1_000;
/// Bars one `data.bars` collects at most, whatever the window.
const MAX_BARS: usize = 100_000;

pub type Bar = [f64; 6];

/// A resolution OpenD serves, and the bar's length in seconds (a day for "1D").
pub fn kl_type(resolution: &str) -> Option<(i32, u32)> {
    Some(match resolution.trim().to_ascii_uppercase().as_str() {
        "1" => (proto::KL_1MIN, 60),
        "3" => (10, 180),
        "5" => (6, 300),
        "15" => (7, 900),
        "30" => (8, 1_800),
        "60" => (9, 3_600),
        "1D" => (2, 86_400),
        _ => return None,
    })
}

pub const INTERVALS: [&str; 7] = ["1", "3", "5", "15", "30", "60", "1D"];

/// A K-line's wall time in `market` as UNIX seconds.
pub fn unix_of(market: Market, time: &str) -> Option<i64> {
    let t = NaiveDateTime::parse_from_str(time.trim(), "%Y-%m-%d %H:%M:%S").ok()?;
    market.tz().from_local_datetime(&t).earliest().map(|d| d.timestamp())
}

/// UNIX seconds as `market`'s wall time, in OpenD's request format.
pub fn wall_of(market: Market, unix: i64) -> String {
    let d = market.tz().timestamp_opt(unix, 0).single().unwrap_or_else(|| market.tz().timestamp_opt(0, 0).unwrap());
    d.format("%Y-%m-%d %H:%M:%S").to_string()
}

/// The bars are stamped at their end: one falls on a session close, or one an interval after an open while none
/// falls on an open. Bars stamped at their start never sit on a close of the regular session.
pub fn end_stamped(market: Market, times: &[i64], secs: u32) -> bool {
    let (opens, closes) = market.bounds();
    let tod = |t: i64| market.tz().timestamp_opt(t, 0).single().map(|d| d.num_seconds_from_midnight());
    let tods: Vec<u32> = times.iter().filter_map(|&t| tod(t)).collect();
    let on_close = tods.iter().any(|t| closes.contains(t));
    let on_open = tods.iter().any(|t| opens.contains(t));
    let after_open = tods.iter().any(|t| opens.iter().any(|o| *t == o + secs));
    on_close || (after_open && !on_open)
}

/// OpenD's K-lines as the app's bars, oldest first, start-stamped, without blanks.
pub fn to_bars(market: Market, secs: u32, lines: &[KLine]) -> Vec<Bar> {
    let mut bars: Vec<Bar> = lines
        .iter()
        .filter(|k| !k.is_blank)
        .filter_map(|k| {
            let t = unix_of(market, &k.time)?;
            let b = [t as f64, k.open_price, k.high_price, k.low_price, k.close_price, k.volume];
            b.iter().all(|x| x.is_finite()).then_some(b)
        })
        .collect();
    if secs < 86_400 {
        let times: Vec<i64> = bars.iter().map(|b| b[0] as i64).collect();
        if end_stamped(market, &times, secs) {
            bars.iter_mut().for_each(|b| b[0] -= secs as f64);
        }
    }
    bars.sort_by(|a, b| a[0].total_cmp(&b[0]));
    bars.dedup_by(|b, a| a[0] == b[0]);
    bars
}

/// The newest `count` bars of `bars` in [`from`, `to`].
pub fn window(mut bars: Vec<Bar>, from: f64, to: f64, count: usize) -> Vec<Bar> {
    bars.retain(|b| b[0] >= from && b[0] <= to);
    let cut = bars.len().saturating_sub(count);
    bars.split_off(cut)
}

/// `data.bars`: forward-adjusted K-lines of `sec` over [`from`, `to`], every page, the newest `count` of them.
pub async fn fetch(
    client: &Client,
    pace: &Pace,
    market: Market,
    sec: &Security,
    resolution: &str,
    (from, to, count): (f64, f64, usize),
) -> Result<Vec<Bar>, String> {
    let Some((kl, secs)) = kl_type(resolution) else {
        return Ok(Vec::new());
    };
    // An end-stamped bar that starts at `to` is written an interval later.
    let (begin, end) = (wall_of(market, from as i64), wall_of(market, to as i64 + secs as i64));
    let mut lines: Vec<KLine> = Vec::new();
    let mut next: Option<Value> = None;
    loop {
        // Only a first page counts toward OpenD's limit.
        if next.is_none() {
            pace.wait().await;
        }
        let mut c2s = json!({
            "rehabType": proto::REHAB_FORWARD, "klType": kl, "security": sec,
            "beginTime": begin, "endTime": end, "maxAckKLNum": PAGE,
        });
        if let Some(key) = next.take() {
            c2s["nextReqKey"] = key;
        }
        let s2c = client.request(proto::QOT_HISTORY_KL, c2s).await?;
        let page: Vec<KLine> = serde_json::from_value(s2c.get("klList").cloned().unwrap_or(json!([])))
            .map_err(|e| format!("OpenD sent unreadable bars: {e}"))?;
        lines.extend(page);
        next = s2c.get("nextReqKey").filter(|k| k.as_str().is_some_and(|s| !s.is_empty())).cloned();
        if next.is_none() || lines.len() >= MAX_BARS {
            break;
        }
    }
    Ok(window(to_bars(market, secs, &lines), from, to, count))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(time: &str, close: f64) -> KLine {
        serde_json::from_value(json!({"time": time, "isBlank": false, "openPrice": close, "highPrice": close,
            "lowPrice": close, "closePrice": close, "volume": "10"}))
        .unwrap()
    }

    #[test]
    fn resolutions_map_to_kline_types() {
        let types: Vec<i32> = INTERVALS.iter().map(|r| kl_type(r).unwrap().0).collect();
        assert_eq!(types, [1, 10, 6, 7, 8, 9, 2]);
        assert_eq!(kl_type("1d"), Some((2, 86_400)));
        for r in ["2", "1W", "1M", "15S", ""] {
            assert_eq!(kl_type(r), None, "{r}");
        }
    }

    #[test]
    fn end_stamped_minutes_move_back_to_their_start() {
        let lines = [line("2025-05-09 09:31:00", 1.0), line("2025-05-09 09:32:00", 2.0), line("2025-05-09 16:00:00", 3.0)];
        let bars = to_bars(Market::Us, 60, &lines);
        let open = unix_of(Market::Us, "2025-05-09 09:30:00").unwrap() as f64;
        assert_eq!(bars[0][0], open);
        assert_eq!(bars[2][0], unix_of(Market::Us, "2025-05-09 15:59:00").unwrap() as f64);
    }

    #[test]
    fn start_stamped_minutes_stay() {
        let lines = [line("2025-05-09 09:30:00", 1.0), line("2025-05-09 09:31:00", 2.0), line("2025-05-09 15:59:00", 3.0)];
        let bars = to_bars(Market::Us, 60, &lines);
        assert_eq!(bars[0][0], unix_of(Market::Us, "2025-05-09 09:30:00").unwrap() as f64);
        assert_eq!(bars.len(), 3);
    }

    #[test]
    fn a_daily_bar_starts_at_midnight_in_new_york() {
        let bars = to_bars(Market::Us, 86_400, &[line("2025-05-09 00:00:00", 1.0)]);
        assert_eq!(bars[0][0], 1_746_763_200.0);
    }

    #[test]
    fn hong_kong_afternoon_end_stamps_are_seen() {
        let times = [unix_of(Market::Hk, "2025-05-09 13:05:00").unwrap(), unix_of(Market::Hk, "2025-05-09 13:10:00").unwrap()];
        assert!(end_stamped(Market::Hk, &times, 300));
        let times = [unix_of(Market::Hk, "2025-05-09 13:00:00").unwrap(), unix_of(Market::Hk, "2025-05-09 13:05:00").unwrap()];
        assert!(!end_stamped(Market::Hk, &times, 300));
    }

    #[test]
    fn blanks_go_and_the_newest_count_stay() {
        let mut blank = line("2025-05-09 00:00:00", 1.0);
        blank.is_blank = true;
        let lines = [blank, line("2025-05-12 00:00:00", 2.0), line("2025-05-13 00:00:00", 3.0), line("2025-05-14 00:00:00", 4.0)];
        let bars = window(to_bars(Market::Us, 86_400, &lines), 0.0, f64::MAX, 2);
        let closes: Vec<f64> = bars.iter().map(|b| b[4]).collect();
        assert_eq!(closes, [3.0, 4.0]);
    }

    #[test]
    fn wall_time_round_trips() {
        let t = unix_of(Market::Sh, "2025-05-09 09:30:00").unwrap();
        assert_eq!(wall_of(Market::Sh, t), "2025-05-09 09:30:00");
    }
}
