/*
 * `data.bars` over the store. The store answers what it covers. A part of the window with no trading time (night,
 * weekend) needs no bars. During a session, a 3/5/15/30-minute chart's newest part is rolled up from the 1-minute
 * bars the stream wrote. Only what is left is asked of OpenD: from one bar before it (to see whether a split or
 * dividend changed the adjusted prices) to the window's end, and what was asked is covered up to the bars still
 * forming. When OpenD refuses (quota, no connection), the stored bars are answered.
 */
use crate::bars::{self, kl_type, Bar};
use crate::hours::{has_trading, may_hold_bars};
use crate::market::Market;
use crate::opend::proto::{KLine, Security};
use crate::store::{Key, Store};
use std::sync::Arc;

/// Where bars come from when the store lacks them.
pub trait Source: Send + Sync {
    /// K-lines of `sec` between two wall times of its market, every page.
    fn klines(
        &self,
        sec: &Security,
        kl: i32,
        extended: bool,
        begin: String,
        end: String,
    ) -> impl std::future::Future<Output = Result<Vec<KLine>, String>> + Send;
}

/// Sizes rolled up from streamed 1-minute bars: they divide every session open's minute (09:30, 13:00).
const ROLLED: [u32; 4] = [180, 300, 900, 1_800];

pub struct History<S> {
    pub source: S,
    pub store: Arc<Store>,
}

pub struct Ask<'a> {
    pub market: Market,
    pub sec: &'a Security,
    pub symbol: &'a str,
    pub resolution: &'a str,
    pub from: f64,
    pub to: f64,
    pub count: usize,
}

/// Bars rolled up into `secs` buckets on the UTC clock grid (every market's offset is whole hours).
pub fn roll_up(bars: &[Bar], secs: u32) -> Vec<Bar> {
    let mut out: Vec<Bar> = Vec::new();
    for b in bars {
        let t = ((b[0] as i64).div_euclid(secs as i64) * secs as i64) as f64;
        match out.last_mut() {
            Some(last) if last[0] == t => {
                last[2] = last[2].max(b[2]);
                last[3] = last[3].min(b[3]);
                last[4] = b[4];
                last[5] += b[5];
            }
            _ => out.push([t, b[1], b[2], b[3], b[4], b[5]]),
        }
    }
    out
}

impl<S: Source> History<S> {
    pub fn extended(market: Market, secs: u32) -> bool {
        market.extended().is_some() && secs < 86_400
    }

    pub async fn bars(&self, a: Ask<'_>, now: i64) -> Result<Vec<Bar>, String> {
        let Some((kl, secs)) = kl_type(a.resolution) else {
            return Ok(Vec::new());
        };
        let ext = Self::extended(a.market, secs);
        let key = Key::new(a.symbol, a.resolution, ext);
        let (from, to) = (a.from.max(0.0) as i64, (a.to as i64).min(now));
        let open_gaps = |missing: Vec<(i64, i64)>| -> Vec<(i64, i64)> {
            missing.into_iter().filter(|&(x, y)| may_hold_bars(a.market, ext, secs, x, y)).collect()
        };
        let gaps = open_gaps(self.store.read(&key, |s| s.missing(from, to)).await);
        if gaps.is_empty() {
            return Ok(bars::window(self.store.read(&key, |s| s.window(from, to)).await, a.from, a.to, a.count));
        }
        if let Some(rolled) = self.rolled(&a, ext, secs, &gaps).await {
            let mut all = self.store.read(&key, |s| s.window(from, to)).await;
            all.retain(|b| !gaps.iter().any(|&(x, y)| b[0] as i64 >= x && b[0] as i64 <= y));
            all.extend(rolled);
            all.sort_by(|x, y| x[0].total_cmp(&y[0]));
            return Ok(bars::window(all, a.from, a.to, a.count));
        }
        match self.fetch(&a, &key, kl, secs, ext, gaps[0].0, to, now).await {
            Ok(()) => Ok(bars::window(self.store.read(&key, |s| s.window(from, to)).await, a.from, a.to, a.count)),
            Err(e) => {
                let stored = self.store.read(&key, |s| s.window(from, to)).await;
                if stored.is_empty() {
                    Err(e)
                } else {
                    println!("[warn] {}: stored bars only ({e})", a.symbol);
                    Ok(bars::window(stored, a.from, a.to, a.count))
                }
            }
        }
    }

    /// The gaps' bars rolled up from the stored 1-minute bars, when those cover every gap.
    async fn rolled(&self, a: &Ask<'_>, ext: bool, secs: u32, gaps: &[(i64, i64)]) -> Option<Vec<Bar>> {
        if !ROLLED.contains(&secs) {
            return None;
        }
        let minutes = Key::new(a.symbol, "1", ext);
        // A bucket starting at x needs the minutes up to x + secs - 1.
        let spans: Vec<(i64, i64)> = gaps.iter().map(|&(x, y)| (x, y + secs as i64 - 1)).collect();
        let covered = self
            .store
            .read(&minutes, |s| spans.iter().all(|&(x, y)| s.missing(x, y).into_iter().all(|(p, q)| !has_trading(a.market, ext, p, q + 59))))
            .await;
        if !covered {
            return None;
        }
        let lines = self.store.read(&minutes, |s| spans.iter().flat_map(|&(x, y)| s.window(x, y)).collect::<Vec<_>>()).await;
        Some(roll_up(&lines, secs).into_iter().filter(|b| gaps.iter().any(|&(x, y)| b[0] as i64 >= x && b[0] as i64 <= y)).collect())
    }

    /// Asks OpenD for whatever of the window `a` the store lacks, and stores it: whether OpenD was asked. Unlike
    /// `bars`, a refusal is the answer.
    pub async fn fill(&self, a: &Ask<'_>, now: i64) -> Result<bool, String> {
        let Some((kl, secs)) = kl_type(a.resolution) else {
            return Err(format!("{} is not a resolution OpenD serves ({})", a.resolution, bars::INTERVALS.join(", ")));
        };
        let ext = Self::extended(a.market, secs);
        let key = Key::new(a.symbol, a.resolution, ext);
        let (from, to) = (a.from.max(0.0) as i64, (a.to as i64).min(now));
        let gaps: Vec<(i64, i64)> = self
            .store
            .read(&key, |s| s.missing(from, to))
            .await
            .into_iter()
            .filter(|&(x, y)| may_hold_bars(a.market, ext, secs, x, y))
            .collect();
        let Some(&(first, _)) = gaps.first() else { return Ok(false) };
        self.fetch(a, &key, kl, secs, ext, first, to, now).await?;
        Ok(true)
    }

    /// The stored bars of `symbol` at `resolution` in [from, to].
    pub async fn stored(&self, market: Market, symbol: &str, resolution: &str, from: i64, to: i64) -> Vec<Bar> {
        let Some((_, secs)) = kl_type(resolution) else { return Vec::new() };
        let key = Key::new(symbol, resolution, Self::extended(market, secs));
        self.store.read(&key, |s| s.window(from, to)).await
    }

    #[allow(clippy::too_many_arguments)]
    async fn fetch(&self, a: &Ask<'_>, key: &Key, kl: i32, secs: u32, ext: bool, gap_start: i64, to: i64, now: i64) -> Result<(), String> {
        let begin = gap_start - secs as i64;
        // An end-stamped bar that starts at `to` is written an interval later.
        let end = to + secs as i64;
        let lines = self
            .source
            .klines(a.sec, kl, ext, bars::wall_of(a.market, begin), bars::wall_of(a.market, end))
            .await?;
        let fetched = bars::to_bars(a.market, secs, ext, &lines);
        // Bars still forming at `now` are not final: they are asked again next time. An answer cut at the most bars
        // one request collects covers only up to its newest bar.
        let cut = |bars: &[Bar], full: bool| match bars.last() {
            Some(last) if full => (last[0] as i64).min(to),
            _ => to.min(now - secs as i64),
        };
        let covered_to = cut(&fetched, lines.len() >= bars::MAX_BARS);
        // The adjusted prices changed under the stored ones: start again from this answer, over the whole window.
        let changed = self
            .store
            .read(key, |s| fetched.iter().any(|b| s.covered_at(b[0] as i64) && s.at(b[0] as i64).is_some_and(|old| (old[4] - b[4]).abs() > 1e-9 * b[4].abs().max(1.0))))
            .await;
        if changed && begin > a.from as i64 {
            println!("[info] {}: adjusted prices changed, its stored {} bars are fetched again", a.symbol, a.resolution);
            self.store.update(key, |s| s.clear()).await;
            let lines = self
                .source
                .klines(a.sec, kl, ext, bars::wall_of(a.market, a.from as i64), bars::wall_of(a.market, end))
                .await?;
            let all = bars::to_bars(a.market, secs, ext, &lines);
            let covered_to = cut(&all, lines.len() >= bars::MAX_BARS);
            self.store.update(key, |s| {
                s.merge(&all);
                s.cover(a.from as i64, covered_to);
            }).await;
        } else {
            self.store.update(key, |s| {
                if changed {
                    s.clear();
                }
                s.merge(&fetched);
                s.cover(begin, covered_to);
            }).await;
        }
        self.store.flush().await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bars::unix_of;
    use crate::store::Backend;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    /// Answers `lines`, and counts the asks.
    struct Fake {
        lines: Mutex<Vec<(String, f64)>>,
        asks: AtomicUsize,
        fail: bool,
    }

    impl Fake {
        fn new(lines: &[(&str, f64)]) -> Self {
            Fake { lines: Mutex::new(lines.iter().map(|(t, c)| (t.to_string(), *c)).collect()), asks: AtomicUsize::new(0), fail: false }
        }
    }

    impl Source for Fake {
        async fn klines(&self, _: &Security, _: i32, _: bool, begin: String, end: String) -> Result<Vec<KLine>, String> {
            self.asks.fetch_add(1, Ordering::Relaxed);
            if self.fail {
                return Err("quota exceeded".into());
            }
            let lines = self.lines.lock().unwrap().clone();
            Ok(lines
                .into_iter()
                .filter(|(t, _)| *t >= begin && *t <= end)
                .map(|(t, c)| serde_json::from_value(json!({"time": t, "isBlank": false, "openPrice": c, "highPrice": c, "lowPrice": c, "closePrice": c, "volume": "1"})).unwrap())
                .collect())
        }
    }

    fn history(fake: Fake) -> History<Fake> {
        History { source: fake, store: Arc::new(Store::new(Backend::Memory)) }
    }

    fn us(t: &str) -> i64 {
        unix_of(Market::Us, t).unwrap()
    }

    fn sec() -> Security {
        Security { market: 11, code: "AAPL".into() }
    }

    fn ask<'a>(sec: &'a Security, res: &'a str, from: i64, to: i64) -> Ask<'a> {
        Ask { market: Market::Us, sec, symbol: "US:AAPL", resolution: res, from: from as f64, to: to as f64, count: 10_000 }
    }

    const DAYS: [(&str, f64); 3] = [("2025-05-07 00:00:00", 1.0), ("2025-05-08 00:00:00", 2.0), ("2025-05-09 00:00:00", 3.0)];

    #[tokio::test]
    async fn a_covered_window_is_answered_without_opend() {
        let h = history(Fake::new(&DAYS));
        let sec = sec();
        let saturday = us("2025-05-10 12:00:00");
        let first = h.bars(ask(&sec, "1D", us("2025-05-07 00:00:00"), saturday), saturday).await.unwrap();
        assert_eq!(first.len(), 3);
        let again = h.bars(ask(&sec, "1D", us("2025-05-07 00:00:00"), saturday), saturday).await.unwrap();
        assert_eq!((again, h.source.asks.load(Ordering::Relaxed)), (first, 1));
    }

    #[tokio::test]
    async fn the_weekend_after_a_fetch_needs_no_opend() {
        let h = history(Fake::new(&[("2025-05-09 15:58:00", 1.0), ("2025-05-09 15:59:00", 2.0)]));
        let sec = sec();
        let friday_night = us("2025-05-09 21:00:00");
        h.bars(ask(&sec, "1", us("2025-05-09 15:58:00"), friday_night), friday_night).await.unwrap();
        let sunday = us("2025-05-11 12:00:00");
        let bars = h.bars(ask(&sec, "1", us("2025-05-09 15:58:00"), sunday), sunday).await.unwrap();
        assert_eq!((bars.len(), h.source.asks.load(Ordering::Relaxed)), (2, 1));
    }

    #[tokio::test]
    async fn a_longer_window_asks_from_one_bar_before_the_covered_end() {
        let h = history(Fake::new(&DAYS[..2]));
        let sec = sec();
        let thursday = us("2025-05-08 23:00:00");
        h.bars(ask(&sec, "1D", us("2025-05-07 00:00:00"), thursday), thursday).await.unwrap();
        h.source.lines.lock().unwrap().push((DAYS[2].0.to_string(), DAYS[2].1));
        let saturday = us("2025-05-10 12:00:00");
        let bars = h.bars(ask(&sec, "1D", us("2025-05-07 00:00:00"), saturday), saturday).await.unwrap();
        assert_eq!(bars.iter().map(|b| b[4]).collect::<Vec<_>>(), [1.0, 2.0, 3.0]);
        assert_eq!(h.source.asks.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn changed_adjusted_prices_drop_the_stored_series() {
        let h = history(Fake::new(&DAYS[..2]));
        let sec = sec();
        let thursday = us("2025-05-08 23:00:00");
        h.bars(ask(&sec, "1D", us("2025-05-07 00:00:00"), thursday), thursday).await.unwrap();
        // A split halves every earlier price.
        *h.source.lines.lock().unwrap() = vec![(DAYS[0].0.into(), 0.5), (DAYS[1].0.into(), 1.0), (DAYS[2].0.into(), 1.5)];
        let saturday = us("2025-05-10 12:00:00");
        let bars = h.bars(ask(&sec, "1D", us("2025-05-07 00:00:00"), saturday), saturday).await.unwrap();
        assert_eq!(bars.iter().map(|b| b[4]).collect::<Vec<_>>(), [0.5, 1.0, 1.5]);
        // The second ask already spans the whole window: it is the new series.
        assert_eq!(h.source.asks.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn opend_refusing_answers_the_stored_bars() {
        let h = history(Fake::new(&DAYS[..2]));
        let sec = sec();
        let thursday = us("2025-05-08 23:00:00");
        h.bars(ask(&sec, "1D", us("2025-05-07 00:00:00"), thursday), thursday).await.unwrap();
        let mut h = h;
        h.source.fail = true;
        let saturday = us("2025-05-10 12:00:00");
        assert_eq!(h.bars(ask(&sec, "1D", us("2025-05-07 00:00:00"), saturday), saturday).await.unwrap().len(), 2);
        let other = Security { market: 11, code: "MSFT".into() };
        let none = Ask { symbol: "US:MSFT", ..ask(&other, "1D", us("2025-05-07 00:00:00"), saturday) };
        assert_eq!(h.bars(none, saturday).await, Err("quota exceeded".into()));
    }

    #[tokio::test]
    async fn streamed_minutes_answer_minute_and_five_minute_charts() {
        let h = history(Fake::new(&[]));
        let sec = sec();
        let (open, now) = (us("2025-05-09 10:00:00"), us("2025-05-09 10:10:30"));
        let minutes = Key::new("US:AAPL", "1", true);
        h.store.update(&minutes, |s| {
            let bars: Vec<Bar> = (0..10).map(|i| [(open + i * 60) as f64, 1.0 + i as f64, 2.0 + i as f64, 0.5, 1.5 + i as f64, 10.0]).collect();
            s.merge(&bars);
            s.cover(open, open + 10 * 60 - 1);
        }).await;
        let ones = h.bars(ask(&sec, "1", open, now - 60), now).await.unwrap();
        assert_eq!(ones.len(), 10);
        let fives = h.bars(ask(&sec, "5", open, open + 5 * 60), now).await.unwrap();
        assert_eq!(fives.len(), 2);
        assert_eq!((fives[0][1], fives[0][2], fives[0][4], fives[0][5]), (1.0, 6.0, 5.5, 50.0));
        assert_eq!(h.source.asks.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn an_answer_cut_at_the_most_bars_covers_only_up_to_its_newest() {
        // As many minutes as one request collects, from Monday 04:00 on; the window reaches well past them.
        let start = us("2025-05-05 04:00:00");
        let times: Vec<String> = (0..bars::MAX_BARS as i64).map(|i| bars::wall_of(Market::Us, start + i * 60)).collect();
        let lines: Vec<(&str, f64)> = times.iter().map(|t| (t.as_str(), 1.0)).collect();
        let h = history(Fake::new(&lines));
        let sec = sec();
        let (to, now) = (start + 200_000 * 60, start + 300_000 * 60);
        assert!(h.fill(&ask(&sec, "1", start, to), now).await.unwrap());
        let newest = h.stored(Market::Us, "US:AAPL", "1", start, to).await.last().unwrap()[0] as i64;
        let key = Key::new("US:AAPL", "1", true);
        let missing = h.store.read(&key, |s| s.missing(start, to)).await;
        assert_eq!(missing.first().map(|m| m.0), Some(newest + 1), "the rest is asked again");
    }

    #[test]
    fn roll_up_keeps_open_high_low_close_and_volume() {
        let b = |t: f64, o: f64, h: f64, l: f64, c: f64| [t, o, h, l, c, 1.0];
        let out = roll_up(&[b(0.0, 1.0, 3.0, 1.0, 2.0), b(60.0, 2.0, 4.0, 0.5, 3.0), b(300.0, 9.0, 9.0, 9.0, 9.0)], 300);
        assert_eq!(out, [[0.0, 1.0, 4.0, 0.5, 3.0, 2.0], [300.0, 9.0, 9.0, 9.0, 9.0, 1.0]]);
    }
}
