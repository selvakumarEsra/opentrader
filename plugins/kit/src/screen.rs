/*
 * Screener markets (plugins/PROTOCOL.md, data.screen, specs/screener-plugin-markets.md): a data plugin's rows for one market of
 * the app's Stock Screener, answered to `data.screen`.
 *
 *   Quote       a broker's quote of one instrument: the price, the day's bar so far, the previous close
 *   Row/Answer  what `data.screen` answers: every instrument of the market with its quote and values
 *   technicals  the history-derived values of one instrument, from its closed daily bars and the live session's
 *   State       the rows last built, and when the app last asked (the refresh stops when it no longer asks)
 *
 * Values use the screener's field ids and are what the screener shows: the plugin computes them with the live price,
 * the app shows them as given. Definitions (the session S is the newest bar, the live one when it has traded):
 *   SMA n, EMA n     the mean of the last n closes; the EMA seeded with the mean of the first n closes
 *   RSI, RSI7        Wilder's RSI of 14 and 7 closes, its averages seeded with the mean of the first n changes
 *   Perf.W/1M/3M/6M/Y  100 * (price / base - 1), base the close of the last bar dated on or before S minus 7 days,
 *                    1, 3, 6 or 12 months; none when the bars do not reach back that far
 *   Perf.5D          the same against the close 5 bars before S
 *   Perf.YTD         the same against the last close of the year before S's
 *   High/Low.1M/3M/6M, price_52_week_high/low  the highest high and lowest low of the bars dated after S minus the
 *                    period (1, 3, 6 or 12 months), when the bars reach back that far
 *   average_volume_Nd_calc  the mean volume of the last N bars, S included
 *   relative_volume_10d_calc  S's volume over the mean volume of the 10 bars before it
 *   volume_change    100 * (S's volume / the bar before's - 1)
 */
use crate::Bar;
use chrono::{Datelike, Months, NaiveDate, TimeZone};
use chrono_tz::Tz;
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// A broker's quote of one instrument.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Quote {
    /// When the quote was taken (UNIX seconds).
    pub time: i64,
    /// When the instrument last traded (UNIX seconds), if the broker says.
    pub traded: Option<i64>,
    pub price: f64,
    /// The session's open, high and low so far, and volume.
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub volume: f64,
    /// The close of the session before.
    pub prev_close: Option<f64>,
}

/// One instrument of a market, as `data.screen` answers it.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Row {
    pub ticker: String,
    pub name: String,
    pub exchange: String,
    /// `stock`, or `secondary` for another listing of a company already listed (the Primary listing filter).
    #[serde(rename = "type")]
    pub kind: String,
    /// When its values were taken (UNIX ms); 0 without a quote.
    pub time: f64,
    /// It has traded in the session the values show, today's.
    pub live: bool,
    pub price: f64,
    pub change: f64,
    pub change_percent: f64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub volume: f64,
    pub prev_close: Option<f64>,
    /// Screener field id -> value.
    pub values: BTreeMap<String, f64>,
}

/// `data.screen`'s answer.
#[derive(Debug, Clone, Serialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Answer {
    pub market: String,
    pub currency: String,
    pub rows: Vec<Row>,
    /// The field ids the rows may hold.
    pub fields: Vec<String>,
    /// When the rows were last built (UNIX ms); None before the first build.
    pub updated: Option<f64>,
    /// What the plugin is doing, for the screener's status ("Daily history: 812 of 2,040 stocks").
    pub status: Option<String>,
    /// Why the last refresh failed, if it did.
    pub error: Option<String>,
}

/// The ids `technicals` can give.
pub const TECHNICAL_FIELDS: &[&str] = &[
    "SMA5", "SMA10", "SMA20", "SMA30", "SMA50", "SMA100", "SMA200", "EMA5", "EMA9", "EMA10", "EMA20", "EMA21", "EMA30", "EMA50", "EMA100",
    "EMA200", "RSI", "RSI7", "Perf.W", "Perf.5D", "Perf.1M", "Perf.3M", "Perf.6M", "Perf.Y", "Perf.YTD", "High.1M", "Low.1M", "High.3M",
    "Low.3M", "High.6M", "Low.6M", "price_52_week_high", "price_52_week_low", "average_volume_10d_calc", "average_volume_30d_calc",
    "average_volume_60d_calc", "average_volume_90d_calc", "relative_volume_10d_calc", "volume_change",
];

const SMAS: [usize; 7] = [5, 10, 20, 30, 50, 100, 200];
const EMAS: [usize; 9] = [5, 9, 10, 20, 21, 30, 50, 100, 200];

fn mean(v: &[f64]) -> f64 {
    v.iter().sum::<f64>() / v.len() as f64
}

/// EMA of `closes` over `n`, seeded with the mean of the first n.
pub fn ema(closes: &[f64], n: usize) -> Option<f64> {
    if n == 0 || closes.len() < n {
        return None;
    }
    let a = 2.0 / (n as f64 + 1.0);
    Some(closes[n..].iter().fold(mean(&closes[..n]), |e, c| e + a * (c - e)))
}

/// Wilder's RSI of `closes` over `n`.
pub fn rsi(closes: &[f64], n: usize) -> Option<f64> {
    if n == 0 || closes.len() <= n {
        return None;
    }
    let diffs: Vec<f64> = closes.windows(2).map(|w| w[1] - w[0]).collect();
    let nf = n as f64;
    let (mut gain, mut loss) = (mean(&diffs[..n].iter().map(|d| d.max(0.0)).collect::<Vec<_>>()), mean(&diffs[..n].iter().map(|d| (-d).max(0.0)).collect::<Vec<_>>()));
    for d in &diffs[n..] {
        gain = (gain * (nf - 1.0) + d.max(0.0)) / nf;
        loss = (loss * (nf - 1.0) + (-d).max(0.0)) / nf;
    }
    Some(if loss == 0.0 { 100.0 } else { 100.0 - 100.0 / (1.0 + gain / loss) })
}

/// The values of one instrument from its bars (`[time, open, high, low, close, volume]`, oldest first, one per
/// session, the newest being S), each bar dated in `tz`.
pub fn technicals(bars: &[Bar], tz: Tz) -> BTreeMap<String, f64> {
    let mut out = BTreeMap::new();
    let Some(last) = bars.last() else { return out };
    let date = |b: &Bar| tz.timestamp_opt(b[0] as i64, 0).single().map(|d| d.date_naive());
    let Some(s) = date(last) else { return out };
    let dates: Vec<Option<NaiveDate>> = bars.iter().map(date).collect();
    let closes: Vec<f64> = bars.iter().map(|b| b[4]).collect();
    let volumes: Vec<f64> = bars.iter().map(|b| b[5]).collect();
    let price = last[4];
    let mut put = |k: &str, v: Option<f64>| {
        if let Some(v) = v.filter(|v| v.is_finite()) {
            out.insert(k.to_string(), v);
        }
    };
    for n in SMAS {
        put(&format!("SMA{n}"), (closes.len() >= n).then(|| mean(&closes[closes.len() - n..])));
    }
    for n in EMAS {
        put(&format!("EMA{n}"), ema(&closes, n));
    }
    put("RSI", rsi(&closes, 14));
    put("RSI7", rsi(&closes, 7));
    let perf = |base: f64| (base > 0.0).then(|| 100.0 * (price / base - 1.0));
    // The bars reach back to `since` when the first one is dated on or before it.
    let reaches = |since: NaiveDate| dates.first().copied().flatten().is_some_and(|d| d <= since);
    let close_on_or_before = |since: NaiveDate| -> Option<f64> {
        if !reaches(since) {
            return None;
        }
        dates.iter().zip(&closes).rev().find(|(d, _)| d.is_some_and(|d| d <= since)).map(|(_, c)| *c)
    };
    let back = |months: u32| s.checked_sub_months(Months::new(months));
    put("Perf.W", close_on_or_before(s - chrono::Duration::days(7)).and_then(perf));
    put("Perf.5D", (closes.len() > 5).then(|| closes[closes.len() - 6]).and_then(perf));
    for (id, m) in [("Perf.1M", 1), ("Perf.3M", 3), ("Perf.6M", 6), ("Perf.Y", 12)] {
        put(id, back(m).and_then(close_on_or_before).and_then(perf));
    }
    let year_end = NaiveDate::from_ymd_opt(s.year() - 1, 12, 31);
    put("Perf.YTD", year_end.and_then(close_on_or_before).and_then(perf));
    for (hi, lo, m) in [("High.1M", "Low.1M", 1), ("High.3M", "Low.3M", 3), ("High.6M", "Low.6M", 6), ("price_52_week_high", "price_52_week_low", 12)] {
        let Some(since) = back(m).filter(|d| reaches(*d)) else { continue };
        let window: Vec<&Bar> = bars.iter().zip(&dates).filter(|(_, d)| d.is_some_and(|d| d > since)).map(|(b, _)| b).collect();
        put(hi, window.iter().map(|b| b[2]).reduce(f64::max));
        put(lo, window.iter().map(|b| b[3]).reduce(f64::min));
    }
    for n in [10, 30, 60, 90] {
        put(&format!("average_volume_{n}d_calc"), (volumes.len() >= n).then(|| mean(&volumes[volumes.len() - n..])));
    }
    let k = volumes.len();
    put("relative_volume_10d_calc", (k > 10).then(|| mean(&volumes[k - 11..k - 1])).filter(|a| *a > 0.0).map(|a| volumes[k - 1] / a));
    put("volume_change", (k > 1 && volumes[k - 2] > 0.0).then(|| 100.0 * (volumes[k - 1] / volumes[k - 2] - 1.0)));
    out
}

/// The session bars of an instrument for `technicals`: its stored daily bars, and the live session's from `q` when
/// it has traded today (stored bars of today, still forming, are replaced by it). `today` is today's midnight in the
/// exchange's time zone (UNIX seconds).
pub fn session_bars(mut daily: Vec<Bar>, q: Option<&Quote>, today: i64) -> Vec<Bar> {
    match q.filter(|q| traded_today(q, today)) {
        Some(q) => {
            daily.retain(|b| (b[0] as i64) < today);
            daily.push([today as f64, q.open, q.high, q.low, q.price, q.volume]);
        }
        None => daily.retain(|b| (b[0] as i64) < today),
    }
    daily
}

/// The quote is of a trade today.
pub fn traded_today(q: &Quote, today: i64) -> bool {
    q.price > 0.0 && q.traded.unwrap_or(q.time) >= today && q.volume > 0.0
}

/// The row of one instrument: its quote, when there is one, and its values.
pub fn row(ticker: &str, name: &str, kind: &str, q: Option<&Quote>, today: i64, values: BTreeMap<String, f64>) -> Row {
    let exchange = ticker.split_once(':').map_or("", |(e, _)| e).to_string();
    let mut r = Row {
        ticker: ticker.to_string(),
        name: name.to_string(),
        exchange,
        kind: kind.to_string(),
        time: 0.0,
        live: false,
        price: 0.0,
        change: 0.0,
        change_percent: 0.0,
        open: 0.0,
        high: 0.0,
        low: 0.0,
        volume: 0.0,
        prev_close: None,
        values,
    };
    if let Some(q) = q.filter(|q| q.price > 0.0) {
        let prev = q.prev_close.filter(|p| *p > 0.0);
        r.time = q.time as f64 * 1000.0;
        r.live = traded_today(q, today);
        r.price = q.price;
        r.change = prev.map_or(0.0, |p| q.price - p);
        r.change_percent = prev.map_or(0.0, |p| 100.0 * (q.price / p - 1.0));
        r.open = q.open;
        r.high = q.high;
        r.low = q.low;
        r.volume = q.volume;
        r.prev_close = prev;
    }
    r
}

/// How long the rows are kept current after the app last asked.
pub const ASKED_FOR: Duration = Duration::from_secs(120);

/// A market's rows and its refresh's bookkeeping.
#[derive(Default)]
pub struct State {
    pub answer: Mutex<Answer>,
    asked: Mutex<Option<Instant>>,
    running: Mutex<bool>,
}

impl State {
    /// The app asked: the rows to answer, and whether a refresh must be started.
    pub fn ask(&self) -> (Answer, bool) {
        *self.asked.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
        let start = {
            let mut running = self.running.lock().unwrap_or_else(|e| e.into_inner());
            !std::mem::replace(&mut *running, true)
        };
        (self.answer.lock().unwrap_or_else(|e| e.into_inner()).clone(), start)
    }

    /// The refresh goes on: the app asked within `ASKED_FOR`. When it no longer asks, the refresh stops (false).
    pub fn wanted(&self) -> bool {
        let wanted = self.asked.lock().unwrap_or_else(|e| e.into_inner()).is_some_and(|t| t.elapsed() < ASKED_FOR);
        if !wanted {
            *self.running.lock().unwrap_or_else(|e| e.into_inner()) = false;
        }
        wanted
    }

    pub fn update(&self, f: impl FnOnce(&mut Answer)) {
        f(&mut self.answer.lock().unwrap_or_else(|e| e.into_inner()));
    }
}

/// Marks, among stock listings, those of a company already listed on an exchange before it in `order` (by the same
/// ticker): `secondary`. The others stay `stock`.
pub fn listing_kinds<'a>(symbols: impl Iterator<Item = &'a str>, order: &[&str]) -> Vec<&'static str> {
    let symbols: Vec<&str> = symbols.collect();
    let rank = |e: &str| order.iter().position(|x| *x == e).unwrap_or(order.len());
    let mut best: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for s in &symbols {
        let (e, t) = s.split_once(':').unwrap_or(("", s));
        let r = rank(e);
        best.entry(t).and_modify(|b| *b = (*b).min(r)).or_insert(r);
    }
    symbols
        .iter()
        .map(|s| {
            let (e, t) = s.split_once(':').unwrap_or(("", s));
            if best.get(t).is_some_and(|b| *b < rank(e)) { "secondary" } else { "stock" }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono_tz::Asia::Kolkata;

    /// `n` daily bars ending on 14/11/2025 (weekdays), closes `close(i)`, volume `1000 + i`.
    fn days(n: usize, close: impl Fn(usize) -> f64) -> Vec<Bar> {
        let mut out = Vec::new();
        let mut d = NaiveDate::from_ymd_opt(2025, 11, 14).unwrap();
        while out.len() < n {
            if d.weekday().number_from_monday() <= 5 {
                let t = Kolkata.from_local_datetime(&d.and_hms_opt(0, 0, 0).unwrap()).unwrap().timestamp() as f64;
                out.push(t);
            }
            d = d.pred_opt().unwrap();
        }
        out.reverse();
        out.iter().enumerate().map(|(i, t)| {
            let c = close(i);
            [*t, c, c + 1.0, c - 1.0, c, 1000.0 + i as f64]
        })
        .collect()
    }

    fn sma(c: &[f64], n: usize) -> f64 {
        c[c.len() - n..].iter().sum::<f64>() / n as f64
    }

    /// REQ-SCREEN-MKT-004.A1: the values equal those computed from the closes, the live price the newest.
    #[test]
    fn technicals_follow_the_closes() {
        let bars = days(250, |i| 100.0 + (i as f64 * 0.37).sin() * 10.0 + i as f64 * 0.1);
        let closes: Vec<f64> = bars.iter().map(|b| b[4]).collect();
        let v = technicals(&bars, Kolkata);
        let cent = |a: f64, b: f64| (a - b).abs() < 0.005;
        assert!(cent(v["SMA50"], sma(&closes, 50)), "{} {}", v["SMA50"], sma(&closes, 50));
        // EMA 20 by hand: seeded with the first 20 closes' mean.
        let mut e = closes[..20].iter().sum::<f64>() / 20.0;
        for c in &closes[20..] {
            e += 2.0 / 21.0 * (c - e);
        }
        assert!(cent(v["EMA20"], e));
        // RSI 14 by hand.
        let d: Vec<f64> = closes.windows(2).map(|w| w[1] - w[0]).collect();
        let (mut g, mut l) = (d[..14].iter().map(|x| x.max(0.0)).sum::<f64>() / 14.0, d[..14].iter().map(|x| (-x).max(0.0)).sum::<f64>() / 14.0);
        for x in &d[14..] {
            g = (g * 13.0 + x.max(0.0)) / 14.0;
            l = (l * 13.0 + (-x).max(0.0)) / 14.0;
        }
        assert!(cent(v["RSI"], 100.0 - 100.0 / (1.0 + g / l)));
        // 1 month before Fri 14/11/2025 is Tue 14/10/2025: the base is that day's close.
        let base_t = Kolkata.with_ymd_and_hms(2025, 10, 14, 0, 0, 0).unwrap().timestamp() as f64;
        let base = bars.iter().find(|b| b[0] == base_t).unwrap()[4];
        assert!(cent(v["Perf.1M"], 100.0 * (closes[249] / base - 1.0)));
        assert!(cent(v["average_volume_10d_calc"], (240..250).map(|i| 1000.0 + i as f64).sum::<f64>() / 10.0));
        assert!(cent(v["relative_volume_10d_calc"], 1249.0 / ((239..249).map(|i| 1000.0 + i as f64).sum::<f64>() / 10.0)));
    }

    /// REQ-SCREEN-MKT-004.A2: a stock with 30 sessions has the short values only.
    #[test]
    fn a_young_stock_has_the_values_its_bars_allow() {
        let v = technicals(&days(30, |i| 50.0 + i as f64), Kolkata);
        assert!(v.contains_key("SMA20") && v.contains_key("Perf.1M") && v.contains_key("RSI"));
        for k in ["SMA50", "SMA200", "Perf.Y", "Perf.6M", "price_52_week_high", "EMA50"] {
            assert!(!v.contains_key(k), "{k}");
        }
    }

    #[test]
    fn the_live_session_is_the_newest_bar() {
        let today = Kolkata.with_ymd_and_hms(2025, 11, 17, 0, 0, 0).unwrap().timestamp();
        let daily = days(3, |i| 10.0 + i as f64);
        let q = Quote { time: today + 36_000, traded: Some(today + 35_990), price: 20.0, open: 19.0, high: 21.0, low: 18.5, volume: 500.0, prev_close: Some(12.0) };
        let bars = session_bars(daily.clone(), Some(&q), today);
        assert_eq!(bars.len(), 4);
        assert_eq!(bars[3], [today as f64, 19.0, 21.0, 18.5, 20.0, 500.0]);
        // Before the session trades, the bars are the stored ones.
        let before = Quote { traded: Some(today - 60_000), volume: 0.0, ..q };
        assert_eq!(session_bars(daily.clone(), Some(&before), today), daily);
        let r = row("NSE:RELIANCE", "Reliance", "stock", Some(&q), today, BTreeMap::new());
        assert!(r.live && (r.change_percent - 100.0 * (20.0 / 12.0 - 1.0)).abs() < 1e-9 && r.exchange == "NSE");
        let empty = row("BSE:X", "X", "stock", None, today, BTreeMap::new());
        assert_eq!((empty.price, empty.time, empty.live), (0.0, 0.0, false));
    }

    /// REQ-SCREEN-MKT-003.A1/A2: a company on both exchanges is primary on NSE; a BSE-only one stays a stock.
    #[test]
    fn a_second_listing_is_secondary() {
        let kinds = listing_kinds(["NSE:RELIANCE", "BSE:RELIANCE", "BSE:SMALLCO", "NSE:TCS"].into_iter(), &["NSE", "BSE"]);
        assert_eq!(kinds, ["stock", "secondary", "stock", "stock"]);
    }

    #[test]
    fn the_refresh_runs_while_the_app_asks() {
        let s = State::default();
        assert!(s.ask().1);
        assert!(!s.ask().1, "a second ask starts no second refresh");
        assert!(s.wanted());
    }
}
