/*
 * `data.bars` from the bar store: the window is answered from the store's series of (symbol, resolution), and only
 * its gaps that may hold trading time (calendar.rs) are asked of the broker, in the broker's largest allowed steps.
 * What was asked is marked covered up to the start of the forming bar, so the next window of the same time needs no
 * broker. A refused request answers the stored bars when there are any.
 */
use crate::calendar::Calendar;
use crate::catalog::Instrument;
use crate::store::{Key, Store};
use crate::Bar;
use std::future::Future;
use std::sync::Arc;

/// Where missing bars come from: one request within the broker's limits.
pub trait Source: Send + Sync {
    /// The bars of `inst` at `resolution` starting in [from, to], oldest first.
    fn fetch(&self, inst: &Instrument, resolution: &str, from: i64, to: i64) -> impl Future<Output = Result<Vec<Bar>, String>> + Send;

    /// The longest span (seconds) one request may ask at `resolution`; None when it is not served.
    fn step(&self, resolution: &str) -> Option<i64>;

    /// The oldest bar start the broker serves at `resolution` (a limit back from `now`), if any.
    fn oldest(&self, _resolution: &str, _now: i64) -> Option<i64> {
        None
    }
}

/// A resolution's bar length: `"1D"` 86400, minutes otherwise.
pub fn secs_of(resolution: &str) -> Option<u32> {
    match resolution {
        "1D" | "D" => Some(86_400),
        m => m.parse::<u32>().ok().filter(|m| *m > 0).map(|m| m * 60),
    }
}

pub struct History<S> {
    pub source: S,
    pub store: Arc<Store>,
}

/// The newest `count` of `bars` starting in [from, to].
pub fn window(bars: Vec<Bar>, from: i64, to: i64, count: usize) -> Vec<Bar> {
    let mut w: Vec<Bar> = bars.into_iter().filter(|b| (from as f64..=to as f64).contains(&b[0])).collect();
    if count > 0 && w.len() > count {
        w.drain(..w.len() - count);
    }
    w
}

/// The bars asked: those starting in [from, to], the newest `count` of them (0: all).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Span {
    pub from: i64,
    pub to: i64,
    pub count: usize,
}

impl<S: Source> History<S> {
    pub async fn bars(&self, inst: &Instrument, cal: &Calendar, resolution: &str, span: Span, now: i64) -> Result<Vec<Bar>, String> {
        let Span { from, to, count } = span;
        let secs = secs_of(resolution).ok_or_else(|| format!("Resolution {resolution} is not served"))?;
        let step = self.source.step(resolution).ok_or_else(|| format!("Resolution {resolution} is not served"))?;
        let key = Key::new(&inst.symbol, resolution, false);
        let hi = to.min(now);
        // The forming bar stays uncovered: it is asked again until it has closed.
        let forming = if secs >= 86_400 { cal.midnight(now) } else { now - now.rem_euclid(secs as i64) };
        let lo = self.source.oldest(resolution, now).map_or(from, |o| from.max(o));
        let gaps: Vec<(i64, i64)> = if lo <= hi {
            self.store.read(&key, |s| s.missing(lo, hi)).await.into_iter().filter(|&(a, b)| cal.may_hold_bars(secs, a, b)).collect()
        } else {
            Vec::new()
        };
        let mut failed = None;
        // Newest first: a chart's first window is its latest bars.
        'gaps: for &(a, b) in gaps.iter().rev() {
            let mut end = b;
            while end >= a {
                let start = (end - step + 1).max(a);
                match self.source.fetch(inst, resolution, start, end).await {
                    Ok(got) => {
                        let covered_to = end.min(forming - 1);
                        self.store
                            .update(&key, |s| {
                                s.merge(&got);
                                if covered_to >= start {
                                    s.cover(start, covered_to);
                                }
                            })
                            .await;
                        // Nothing in a whole step: the history starts later.
                        if got.is_empty() && start > a && end - start + 1 >= step {
                            self.store.update(&key, |s| s.cover(a, start - 1)).await;
                            break;
                        }
                    }
                    Err(e) => {
                        failed = Some(e);
                        break 'gaps;
                    }
                }
                end = start - 1;
            }
        }
        let bars = self.store.read(&key, |s| s.window(from, to)).await;
        match failed {
            Some(e) if bars.is_empty() => Err(e),
            Some(e) => {
                println!("[warn] {} {resolution}: {e}; answering the stored bars", inst.symbol);
                Ok(window(bars, from, to, count))
            }
            None => Ok(window(bars, from, to, count)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Backend;
    use chrono::TimeZone;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Fake {
        asked: Mutex<Vec<(i64, i64)>>,
        refuse: bool,
    }

    impl Source for Fake {
        async fn fetch(&self, _: &Instrument, _: &str, from: i64, to: i64) -> Result<Vec<Bar>, String> {
            self.asked.lock().unwrap().push((from, to));
            if self.refuse {
                return Err("refused".into());
            }
            // A bar each trading minute 09:15-15:29 is too many for a test: one bar at each 10:00 of the span.
            let mut out = Vec::new();
            let mut t = from - from.rem_euclid(86_400);
            while t <= to {
                let ten = t + 16_200; // 10:00 IST is 04:30 UTC
                if (from..=to).contains(&ten) {
                    out.push([ten as f64, 1.0, 1.0, 1.0, 1.0, 1.0]);
                }
                t += 86_400;
            }
            Ok(out)
        }

        fn step(&self, resolution: &str) -> Option<i64> {
            (resolution == "1").then_some(60 * 86_400)
        }
    }

    fn ist(s: &str) -> i64 {
        chrono_tz::Asia::Kolkata.from_local_datetime(&chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap()).unwrap().timestamp()
    }

    fn history(fake: Fake) -> History<Fake> {
        History { source: fake, store: Arc::new(Store::new(Backend::Memory)) }
    }

    fn inst() -> Instrument {
        Instrument { symbol: "NSE:INFY".into(), ..Default::default() }
    }

    #[tokio::test]
    async fn a_covered_window_needs_no_broker() {
        let h = history(Fake::default());
        let cal = Calendar::india("NSE");
        let (from, to, now) = (ist("2026-11-09 00:00"), ist("2026-11-13 23:59"), ist("2026-11-16 12:00"));
        let first = h.bars(&inst(), &cal, "1", Span { from, to, count: 0 }, now).await.unwrap();
        assert_eq!(first.len(), 5);
        let asked = h.source.asked.lock().unwrap().len();
        let again = h.bars(&inst(), &cal, "1", Span { from, to, count: 0 }, now).await.unwrap();
        assert_eq!((again.len(), h.source.asked.lock().unwrap().len()), (5, asked));
    }

    #[tokio::test]
    async fn a_weekend_gap_is_not_asked() {
        let h = history(Fake::default());
        let cal = Calendar::india("NSE");
        let bars = h.bars(&inst(), &cal, "1", Span { from: ist("2026-11-14 00:00"), to: ist("2026-11-15 23:59"), count: 0 }, ist("2026-11-16 12:00")).await.unwrap();
        assert!(bars.is_empty());
        assert!(h.source.asked.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_long_window_is_asked_in_the_brokers_steps_newest_first() {
        let h = history(Fake::default());
        let cal = Calendar::india("NSE");
        let from = ist("2026-03-01 00:00");
        let to = from + 150 * 86_400 - 1;
        h.bars(&inst(), &cal, "1", Span { from, to, count: 0 }, to + 86_400 * 30).await.unwrap();
        let asked = h.source.asked.lock().unwrap().clone();
        let spans: Vec<i64> = asked.iter().map(|(a, b)| (b - a + 1) / 86_400).collect();
        assert_eq!(spans, [60, 60, 30]);
        assert!(asked[0].0 > asked[1].0, "newest first");
    }

    #[tokio::test]
    async fn the_forming_bar_stays_uncovered_and_a_refusal_answers_the_store() {
        let h = history(Fake::default());
        let cal = Calendar::india("NSE");
        let now = ist("2026-11-13 10:00") + 30;
        h.bars(&inst(), &cal, "1", Span { from: ist("2026-11-13 09:00"), to: now, count: 0 }, now).await.unwrap();
        let key = Key::new("NSE:INFY", "1", false);
        assert!(!h.store.read(&key, |s| s.covered_at(ist("2026-11-13 10:00"))).await);
        assert!(h.store.read(&key, |s| s.covered_at(ist("2026-11-13 09:59"))).await);

        let refusing = History { source: Fake { refuse: true, ..Default::default() }, store: h.store.clone() };
        let later = now + 600;
        let bars = refusing.bars(&inst(), &cal, "1", Span { from: ist("2026-11-13 09:00"), to: later, count: 0 }, later).await.unwrap();
        assert_eq!(bars.len(), 1);
        let empty = History { source: Fake { refuse: true, ..Default::default() }, store: Arc::new(Store::new(Backend::Memory)) };
        assert!(empty.bars(&inst(), &cal, "1", Span { from: ist("2026-11-13 09:00"), to: later, count: 0 }, later).await.is_err());
    }
}
