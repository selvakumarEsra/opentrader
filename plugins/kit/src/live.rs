/*
 * Ticks into quotes and 1-minute bars. A broker's stream gives the last price, the day's volume so far and the
 * day's open/high/low; `Minutes` keeps each symbol's forming minute and, when a tick starts the next minute, hands
 * back the closed one if it was seen from its start (not the minute the stream joined midway, nor one cut by a
 * reconnect). Its volume is the change of the day's volume over the minute. `store_minute` writes it to the store's
 * 1-minute series and marks it covered, so charts read streamed minutes in market hours and the store outside them.
 */
use crate::calendar::Calendar;
use crate::store::{Key, Store};
use crate::Bar;
use serde_json::{json, Value};
use std::collections::HashMap;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Tick {
    pub symbol: String,
    /// The exchange's time of the trade (UNIX seconds).
    pub time: i64,
    pub price: f64,
    /// The day's traded volume so far.
    pub day_volume: Option<f64>,
    pub open: Option<f64>,
    pub high: Option<f64>,
    pub low: Option<f64>,
    pub prev_close: Option<f64>,
}

struct Forming {
    bar: Bar,
    /// Seen from its start.
    whole: bool,
    /// The day's volume when the minute started.
    base: Option<f64>,
    /// The last day's volume seen.
    last_volume: Option<f64>,
}

#[derive(Default)]
struct Day {
    start: i64,
    open: f64,
    high: f64,
    low: f64,
    prev_close: Option<f64>,
}

#[derive(Default)]
pub struct Minutes {
    forming: HashMap<String, Forming>,
    days: HashMap<String, Day>,
}

impl Minutes {
    /// After a reconnect: the minutes in progress missed ticks.
    pub fn reset(&mut self) {
        self.forming.clear();
    }

    /// Takes `t`: the closed whole minute it ends, if any, and its `data.quote`.
    pub fn tick(&mut self, t: &Tick, cal: &Calendar) -> (Option<Bar>, Value) {
        let m = t.time - t.time.rem_euclid(60);
        let p = t.price;
        let mut closed = None;
        match self.forming.get_mut(&t.symbol) {
            None => {
                self.forming.insert(
                    t.symbol.clone(),
                    Forming { bar: [m as f64, p, p, p, p, 0.0], whole: false, base: t.day_volume, last_volume: t.day_volume },
                );
            }
            Some(f) if (m as f64) > f.bar[0] => {
                if f.whole {
                    closed = Some(f.bar);
                }
                // The day's volume starts again each trading day: a new day's first minute starts from none.
                let new_day = cal.midnight(m) != cal.midnight(f.bar[0] as i64);
                let base = if new_day { t.day_volume.map(|_| 0.0) } else { f.last_volume.or(t.day_volume) };
                *f = Forming { bar: [m as f64, p, p, p, p, 0.0], whole: true, base, last_volume: t.day_volume.or(base) };
                if let (Some(v), Some(b)) = (t.day_volume, base) {
                    f.bar[5] = (v - b).max(0.0);
                }
            }
            // A late tick of an earlier minute: the price stands, the bar does not move back.
            Some(f) if (m as f64) < f.bar[0] => {}
            Some(f) => {
                f.bar[2] = f.bar[2].max(p);
                f.bar[3] = f.bar[3].min(p);
                f.bar[4] = p;
                if let Some(v) = t.day_volume {
                    f.last_volume = Some(v);
                    if let Some(b) = f.base {
                        f.bar[5] = (v - b).max(0.0);
                    }
                }
            }
        }
        let start = cal.midnight(t.time);
        let day = self.days.entry(t.symbol.clone()).or_default();
        if day.start != start {
            *day = Day { start, open: t.open.unwrap_or(p), high: p, low: p, prev_close: day.prev_close.or(t.prev_close) };
        }
        day.open = t.open.unwrap_or(day.open);
        day.high = t.high.unwrap_or(day.high.max(p));
        day.low = t.low.unwrap_or(day.low.min(p));
        if t.prev_close.is_some() {
            day.prev_close = t.prev_close;
        }
        let minute = self.forming.get(&t.symbol).map(|f| f.bar).unwrap_or([m as f64, p, p, p, p, 0.0]);
        let quote = json!({
            "ticker": t.symbol, "time": t.time, "price": p, "prevClose": day.prev_close,
            "minute": minute,
            "day": [day.start, day.open, day.high, day.low, p, t.day_volume.unwrap_or(0.0)],
        });
        (closed, quote)
    }
}

/// Writes a closed whole minute of `symbol` to the store's 1-minute series, covered.
pub async fn store_minute(store: &Store, symbol: &str, bar: Bar) {
    let key = Key::new(symbol, "1", false);
    let t = bar[0] as i64;
    store
        .update(&key, |s| {
            s.merge(&[bar]);
            s.cover(t, t + 59);
        })
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Backend;

    fn tick(time: i64, price: f64, volume: f64) -> Tick {
        Tick { symbol: "NSE:INFY".into(), time, price, day_volume: Some(volume), prev_close: Some(99.0), ..Default::default() }
    }

    #[tokio::test]
    async fn the_minute_seen_from_its_start_is_written_not_the_one_joined() {
        let cal = Calendar::india("NSE");
        let mut m = Minutes::default();
        let t0 = 1_763_017_200; // a minute's start
        let mut closed = Vec::new();
        for (t, p, v) in [(t0 + 30, 100.0, 1000.0), (t0 + 61, 101.0, 1100.0), (t0 + 90, 103.0, 1250.0), (t0 + 119, 102.0, 1300.0), (t0 + 125, 104.0, 1400.0)] {
            let (c, q) = m.tick(&tick(t, p, v), &cal);
            assert_eq!(q["prevClose"], 99.0);
            closed.extend(c);
        }
        // The first minute (joined at :30) is not written; the second is, its volume the day's change over it.
        assert_eq!(closed, vec![[(t0 + 60) as f64, 101.0, 103.0, 101.0, 102.0, 300.0]]);
        let store = Store::new(Backend::Memory);
        store_minute(&store, "NSE:INFY", closed[0]).await;
        let key = Key::new("NSE:INFY", "1", false);
        assert!(store.read(&key, |s| s.covered_at(t0 + 60) && s.covered_at(t0 + 119) && !s.covered_at(t0 + 120)).await);
    }

    #[test]
    fn a_new_days_first_minute_counts_from_none() {
        let cal = Calendar::india("NSE");
        let mut m = Minutes::default();
        let evening = 1_763_027_400; // 15:20 IST
        m.tick(&tick(evening, 100.0, 4_000_000.0), &cal);
        m.tick(&tick(evening + 60, 100.0, 5_000_000.0), &cal);
        let morning = evening + 86_400 - 6 * 3600 + 300; // 09:25 IST the next day
        m.tick(&tick(morning, 101.0, 20_000.0), &cal);
        let (closed, _) = m.tick(&tick(morning + 60, 101.0, 30_000.0), &cal);
        assert_eq!(closed.map(|b| b[5]), Some(20_000.0));
    }

    #[test]
    fn a_reconnect_drops_the_minute_in_progress() {
        let cal = Calendar::india("NSE");
        let mut m = Minutes::default();
        let t0 = 1_763_017_200;
        m.tick(&tick(t0 + 30, 100.0, 10.0), &cal);
        m.tick(&tick(t0 + 61, 100.0, 20.0), &cal);
        m.reset();
        let (closed, q) = m.tick(&tick(t0 + 125, 100.0, 30.0), &cal);
        assert_eq!(closed, None);
        assert_eq!(q["minute"][0], (t0 + 120) as f64);
        let (closed, _) = m.tick(&tick(t0 + 185, 100.0, 40.0), &cal);
        assert_eq!(closed, None, "joined midway after the reconnect");
    }
}
