/*
 * Live prices. `Subscriptions` follows the app's `data.subscribe` set within OpenD's rules: a new symbol is
 * subscribed at once (Basic quotes and 1-minute K-lines, 2 quota units); a dropped one only after it has been held
 * a minute, since OpenD refuses an earlier cancel. `Quotes` turns OpenD's pushes into `data.quote` notifications.
 */
use crate::bars::{self, Bar};
use crate::market::{security_of, symbol_of, Market};
use crate::opend::proto::{self, BasicQot, KLine, Security};
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

/// Qot_Sub's request for `symbols`.
pub fn sub_request(symbols: &[String], subscribe: bool) -> Value {
    let list: Vec<Security> = symbols.iter().filter_map(|s| security_of(s).map(|(_, sec)| sec)).collect();
    json!({
        "securityList": list, "subTypeList": [proto::SUB_BASIC, proto::SUB_KL_1MIN],
        "isSubOrUnSub": subscribe, "isRegOrUnRegPush": subscribe, "isFirstPush": true,
    })
}

/// The forming 1-minute bar of each symbol, kept to send with its next price.
#[derive(Default)]
pub struct Quotes {
    minute: HashMap<String, Bar>,
}

impl Quotes {
    /// A 1-minute K-line push. A forming bar stamped after `now` is stamped at its end: its start is a minute earlier.
    pub fn minute_push(&mut self, s2c: &Value, now: i64) {
        let Some(symbol) = s2c.get("security").and_then(|v| serde_json::from_value::<Security>(v.clone()).ok()).and_then(|s| symbol_of(&s)) else {
            return;
        };
        let Some((market, _)) = security_of(&symbol) else { return };
        let lines: Vec<KLine> = serde_json::from_value(s2c.get("klList").cloned().unwrap_or(json!([]))).unwrap_or_default();
        let Some(last) = lines.iter().rev().find(|k| !k.is_blank) else { return };
        let Some(t) = bars::unix_of(market, &last.time) else { return };
        let start = if t > now { t - 60 } else { t };
        self.minute.insert(symbol, [start as f64, last.open_price, last.high_price, last.low_price, last.close_price, last.volume]);
    }

    /// A Basic push: one `data.quote` per wanted symbol in it.
    pub fn basic_push(&self, s2c: &Value, subs: &Subscriptions, now: i64) -> Vec<Value> {
        let list: Vec<BasicQot> = serde_json::from_value(s2c.get("basicQotList").cloned().unwrap_or(json!([]))).unwrap_or_default();
        list.iter()
            .filter_map(|q| {
                let symbol = symbol_of(&q.security)?;
                subs.wants(&symbol).then(|| quote_of(&symbol, q, self.minute.get(&symbol), now))
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
}
