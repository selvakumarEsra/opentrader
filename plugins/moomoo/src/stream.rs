/*
 * Live prices. `Subscriptions` follows the app's `data.subscribe` set within OpenD's rules: a new symbol is
 * subscribed at once (Basic quotes and 1-minute K-lines, 2 quota units); a dropped one only after it has been held
 * a minute, since OpenD refuses an earlier cancel. `Quotes` turns OpenD's pushes into `data.quote` notifications.
 */
use crate::bars::{self, Bar};
use crate::market::{security_of, symbol_of, Market};
use crate::opend::proto::{self, BasicQot, KLine, Security};
use crate::store::{Key, Store};
use chrono::{TimeZone, Timelike};
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};
use std::time::Duration;
use tokio::time::Instant;

pub const HOLD: Duration = Duration::from_secs(60);

#[derive(Default)]
pub struct Subscriptions {
    wanted: BTreeSet<String>,
    /// Subscribed at OpenD, and since when.
    held: HashMap<String, Instant>,
}

impl Subscriptions {
    /// The app's new set; the symbols to subscribe now.
    pub fn set(&mut self, wanted: impl IntoIterator<Item = String>, now: Instant) -> Vec<String> {
        self.wanted = wanted.into_iter().filter(|s| security_of(s).is_some()).collect();
        let new: Vec<String> = self.wanted.iter().filter(|s| !self.held.contains_key(*s)).cloned().collect();
        for s in &new {
            self.held.insert(s.clone(), now);
        }
        new
    }

    /// Dropped symbols held long enough: unsubscribe them now.
    pub fn due(&mut self, now: Instant) -> Vec<String> {
        let due: Vec<String> = self
            .held
            .iter()
            .filter(|(s, since)| !self.wanted.contains(*s) && now.duration_since(**since) >= HOLD)
            .map(|(s, _)| s.clone())
            .collect();
        for s in &due {
            self.held.remove(s);
        }
        due
    }

    /// After a reconnect: the wanted set, subscribed again from `now` (the old connection's are gone).
    pub fn reconnected(&mut self, now: Instant) -> Vec<String> {
        self.held = self.wanted.iter().map(|s| (s.clone(), now)).collect();
        self.wanted.iter().cloned().collect()
    }

    /// OpenD refused them (quota, no rights): not held, so a later set or reconnect asks again.
    pub fn forget(&mut self, symbols: &[String]) {
        for s in symbols {
            self.held.remove(s);
        }
    }

    pub fn wants(&self, symbol: &str) -> bool {
        self.wanted.contains(symbol)
    }
}

/// Qot_Sub's request for `symbols`: Basic quotes and 1-minute K-lines.
pub fn sub_request(symbols: &[String], subscribe: bool) -> Value {
    sub_request_of(symbols, &[proto::SUB_BASIC, proto::SUB_KL_1MIN], subscribe)
}

/// Qot_Sub's request for `symbols` and the sub types `types`.
pub fn sub_request_of(symbols: &[String], types: &[i32], subscribe: bool) -> Value {
    let list: Vec<Security> = symbols.iter().filter_map(|s| security_of(s).map(|(_, sec)| sec)).collect();
    json!({
        "securityList": list, "subTypeList": types,
        "isSubOrUnSub": subscribe, "isRegOrUnRegPush": subscribe, "isFirstPush": true,
        // Pre- and post-market for US symbols; OpenD ignores it for the others.
        "extendedTime": true,
    })
}

/// What closed streamed minutes cover: per symbol, from the start of the last closed minute on this connection to the
/// end of the new one, since the minutes between two closed bars had no trades. After a reconnect the minutes
/// missed are not known, so coverage starts again.
#[derive(Default)]
pub struct MinuteCover {
    last: HashMap<String, i64>,
}

impl MinuteCover {
    /// The span the closed minute of `symbol` starting at `t` covers.
    pub fn span(&mut self, symbol: &str, t: i64) -> (i64, i64) {
        let prev = self.last.insert(symbol.to_string(), t);
        (prev.filter(|p| *p < t).unwrap_or(t), t + 59)
    }

    /// A new connection.
    pub fn reset(&mut self) {
        self.last.clear();
    }

    /// No longer streamed.
    pub fn forget(&mut self, symbol: &str) {
        self.last.remove(symbol);
    }
}

/// A closed streamed 1-minute bar of `symbol`, stored with the span it covers.
pub async fn store_minute(store: &Store, cover: &std::sync::Mutex<MinuteCover>, symbol: &str, bar: Bar) {
    let Some((market, _)) = security_of(symbol) else { return };
    let (a, b) = cover.lock().unwrap_or_else(|e| e.into_inner()).span(symbol, bar[0] as i64);
    let key = Key::new(symbol, "1", market.extended().is_some());
    store
        .update(&key, |s| {
            s.merge(&[bar]);
            s.cover(a, b);
        })
        .await;
}

/// The forming 1-minute bar and the last Basic quote of each symbol, kept to send with its next price.
#[derive(Default)]
pub struct Quotes {
    minute: HashMap<String, Bar>,
    basic: HashMap<String, BasicQot>,
}

impl Quotes {
    /// A 1-minute K-line push. A forming bar stamped after `now` is stamped at its end: its start is a minute earlier.
    /// When the push starts a newer bar, the one it replaces has closed: that one is returned, to be stored.
    pub fn minute_push(&mut self, s2c: &Value, now: i64) -> Option<(String, Bar)> {
        let symbol = s2c.get("security").and_then(|v| serde_json::from_value::<Security>(v.clone()).ok()).and_then(|s| symbol_of(&s))?;
        let (market, _) = security_of(&symbol)?;
        let lines: Vec<KLine> = serde_json::from_value(s2c.get("klList").cloned().unwrap_or(json!([]))).unwrap_or_default();
        let last = lines.iter().rev().find(|k| !k.is_blank)?;
        let t = bars::unix_of(market, &last.time)?;
        let start = if t > now { t - 60 } else { t };
        let bar = [start as f64, last.open_price, last.high_price, last.low_price, last.close_price, last.volume];
        let previous = self.minute.insert(symbol.clone(), bar)?;
        (previous[0] < bar[0]).then_some((symbol, previous))
    }

    /// The `data.quote` of a K-line push, sent so the chart moves without waiting for the next Basic push: the
    /// minute's close is the price, and the day's bar takes the minute's high, low and close. None until a Basic
    /// quote was seen for the symbol (the previous close and the day's bar are not known before).
    pub fn minute_quote(&self, symbol: &str, subs: &Subscriptions, now: i64) -> Option<Value> {
        if !subs.wants(symbol) {
            return None;
        }
        let (basic, minute) = (self.basic.get(symbol)?, self.minute.get(symbol)?);
        let q = BasicQot {
            high_price: basic.high_price.max(minute[2]),
            low_price: if basic.low_price > 0.0 { basic.low_price.min(minute[3]) } else { minute[3] },
            cur_price: minute[4],
            update_timestamp: Some(now as f64),
            ..basic.clone()
        };
        Some(quote_of(symbol, &q, Some(minute), now))
    }

    /// A Basic push: one `data.quote` per wanted symbol in it. Each quote is kept for the K-line pushes after it.
    pub fn basic_push(&mut self, s2c: &Value, subs: &Subscriptions, now: i64) -> Vec<Value> {
        let list: Vec<BasicQot> = serde_json::from_value(s2c.get("basicQotList").cloned().unwrap_or(json!([]))).unwrap_or_default();
        list.into_iter()
            .filter_map(|q| {
                let symbol = symbol_of(&q.security)?;
                if !subs.wants(&symbol) {
                    return None;
                }
                let quote = quote_of(&symbol, &q, self.minute.get(&symbol), now);
                self.basic.insert(symbol, q);
                Some(quote)
            })
            .collect()
    }
}

/// The `data.quote` of a Basic quote: price, previous close, the day's bar and the forming minute bar.
pub fn quote_of(symbol: &str, q: &BasicQot, minute: Option<&Bar>, now: i64) -> Value {
    let market = security_of(symbol).map_or(Market::Us, |(m, _)| m);
    let time = q.update_timestamp.map_or(now, |t| t as i64);
    let midnight = market
        .tz()
        .timestamp_opt(time, 0)
        .single()
        .and_then(|d| d.with_hour(0)?.with_minute(0)?.with_second(0))
        .map_or(time, |d| d.timestamp());
    let mut quote = json!({
        "ticker": symbol, "time": time, "price": q.cur_price, "prevClose": q.last_close_price,
        "day": [midnight, q.open_price, q.high_price, q.low_price, q.cur_price, q.volume],
    });
    if let Some(m) = minute {
        quote["minute"] = json!(m);
    }
    quote
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[tokio::test(start_paused = true)]
    async fn new_symbols_at_once_dropped_ones_after_a_minute() {
        let mut subs = Subscriptions::default();
        let t0 = Instant::now();
        assert_eq!(subs.set(s(&["US:AAPL"]), t0), s(&["US:AAPL"]));
        assert_eq!(subs.set(s(&["US:AAPL", "HK:00700", "XX:NOPE"]), t0), s(&["HK:00700"]));
        subs.set(Vec::new(), t0 + Duration::from_secs(10));
        assert!(subs.due(t0 + Duration::from_secs(59)).is_empty());
        let mut due = subs.due(t0 + HOLD);
        due.sort();
        assert_eq!(due, s(&["HK:00700", "US:AAPL"]));
        assert!(subs.due(t0 + HOLD * 2).is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_reconnect_subscribes_the_wanted_set_again() {
        let mut subs = Subscriptions::default();
        let t0 = Instant::now();
        subs.set(s(&["US:AAPL"]), t0);
        assert_eq!(subs.reconnected(t0), s(&["US:AAPL"]));
        assert!(subs.set(s(&["US:AAPL"]), t0).is_empty());
    }

    #[test]
    fn a_newer_minute_closes_the_one_before() {
        let mut quotes = Quotes::default();
        let now = bars::unix_of(Market::Us, "2025-05-09 10:00:30").unwrap();
        let push = |time: &str, close: f64| json!({"klType":1,"rehabType":1,"security":{"market":11,"code":"AAPL"},
            "klList":[{"time":time,"isBlank":false,"openPrice":1,"highPrice":2,"lowPrice":0.5,"closePrice":close,"volume":"5"}]});
        assert_eq!(quotes.minute_push(&push("2025-05-09 10:00:00", 1.0), now), None);
        assert_eq!(quotes.minute_push(&push("2025-05-09 10:00:00", 1.2), now), None);
        let (sym, closed) = quotes.minute_push(&push("2025-05-09 10:01:00", 1.3), now + 60).unwrap();
        assert_eq!((sym.as_str(), closed[0] as i64, closed[4]), ("US:AAPL", now - 30, 1.2));
    }

    #[test]
    fn closed_minutes_cover_the_span_since_the_last_one_until_a_reconnect() {
        let mut cover = MinuteCover::default();
        assert_eq!(cover.span("US:AAPL", 600), (600, 659));
        assert_eq!(cover.span("US:AAPL", 780), (600, 839));
        cover.reset();
        assert_eq!(cover.span("US:AAPL", 900), (900, 959));
        cover.forget("US:AAPL");
        assert_eq!(cover.span("US:AAPL", 1020), (1020, 1079));
    }

    #[tokio::test(start_paused = true)]
    async fn basic_pushes_become_quotes_of_wanted_symbols_only() {
        let mut subs = Subscriptions::default();
        subs.set(s(&["US:AAPL"]), Instant::now());
        let mut quotes = Quotes::default();
        let now = bars::unix_of(Market::Us, "2025-05-09 10:00:30").unwrap();
        quotes.minute_push(&json!({"klType":1,"rehabType":1,"security":{"market":11,"code":"AAPL"},
            "klList":[{"time":"2025-05-09 10:01:00","isBlank":false,"openPrice":200,"highPrice":201,"lowPrice":199,"closePrice":200.5,"volume":"900"}]}), now);
        let qot = |code: &str| json!({"security":{"market":11,"code":code},"isSuspended":false,"listTime":"","priceSpread":0.01,
            "updateTime":"","highPrice":202,"openPrice":198,"lowPrice":197,"curPrice":200.5,"lastClosePrice":196,
            "volume":"1000000","turnover":0,"turnoverRate":0,"amplitude":0,"updateTimestamp":now as f64});
        let sent = quotes.basic_push(&json!({"basicQotList":[qot("AAPL"), qot("MSFT")]}), &subs, now);
        assert_eq!(sent.len(), 1);
        let q = &sent[0];
        assert_eq!((q["ticker"].as_str(), q["price"].as_f64(), q["prevClose"].as_f64()), (Some("US:AAPL"), Some(200.5), Some(196.0)));
        let midnight = bars::unix_of(Market::Us, "2025-05-09 00:00:00").unwrap();
        assert_eq!(q["day"], json!([midnight, 198.0, 202.0, 197.0, 200.5, 1_000_000.0]));
        let minute_start = bars::unix_of(Market::Us, "2025-05-09 10:00:00").unwrap() as f64;
        assert_eq!(q["minute"][0].as_f64(), Some(minute_start));
    }

    // REQ-MOOMOO-005.A4
    #[tokio::test(start_paused = true)]
    async fn a_minute_push_after_a_basic_one_moves_the_price() {
        let mut subs = Subscriptions::default();
        subs.set(s(&["US:AAPL"]), Instant::now());
        let mut quotes = Quotes::default();
        let now = bars::unix_of(Market::Us, "2025-05-09 10:00:30").unwrap();
        let kl = |high: f64, low: f64, close: f64| json!({"klType":1,"rehabType":1,"security":{"market":11,"code":"AAPL"},
            "klList":[{"time":"2025-05-09 10:01:00","isBlank":false,"openPrice":200,"highPrice":high,"lowPrice":low,"closePrice":close,"volume":"900"}]});
        quotes.minute_push(&kl(201.0, 199.0, 200.5), now);
        assert_eq!(quotes.minute_quote("US:AAPL", &subs, now), None);
        let qot = json!({"security":{"market":11,"code":"AAPL"},"highPrice":202,"openPrice":198,"lowPrice":197,"curPrice":200.5,
            "lastClosePrice":196,"volume":"1000000","updateTimestamp":now as f64});
        quotes.basic_push(&json!({"basicQotList":[qot]}), &subs, now);
        quotes.minute_push(&kl(203.0, 199.0, 202.5), now + 10);
        let q = quotes.minute_quote("US:AAPL", &subs, now + 10).unwrap();
        assert_eq!((q["price"].as_f64(), q["prevClose"].as_f64(), q["time"].as_i64()), (Some(202.5), Some(196.0), Some(now + 10)));
        let midnight = bars::unix_of(Market::Us, "2025-05-09 00:00:00").unwrap();
        assert_eq!(q["day"], json!([midnight, 198.0, 203.0, 197.0, 202.5, 1_000_000.0]));
        assert_eq!(q["minute"][4].as_f64(), Some(202.5));
        assert_eq!(quotes.minute_quote("US:MSFT", &subs, now + 10), None);
    }
}
