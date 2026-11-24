/*
 * The data plugins' live quotes, batched for the views (specs/performance.md REQ-PERF-003).
 *
 * A busy symbol would send the views one event per quote, each a JSON message every page parses. The quotes are
 * kept here, the newest per symbol, and sent once per flush: bars about once a display frame, trades every 100 ms.
 * A bar of another time first sends the one it follows, so a closed bar keeps its last values. A bar goes only to the
 * windows whose charts show its symbol (their charts are its one reader); trades go to every window.
 *
 * Kept apart from data.rs, which only queues a quote (`QuoteBatch::add`) and names a symbol's chart windows.
 */
use crate::data::types::{ChartAggregate, TradeTick};
use opentrader_core::sync::LockExt;
use std::collections::{BTreeSet, HashMap};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::AppHandle;
use tauri_specta::Event;

/// How often the plugins' bars go to the charts: about one display frame (the app's own stream's flush).
pub const BAR_FLUSH: Duration = Duration::from_millis(16);
/// How often their trades go to the views: the watchlist and the panels read the last price.
pub const TRADE_FLUSH: Duration = Duration::from_millis(100);

/// The quotes since the last flush, the newest per symbol.
#[derive(Default)]
pub struct QuoteBatch {
    bars: HashMap<String, ChartAggregate>,
    trades: HashMap<String, TradeTick>,
    /// When the trades were last sent (None: never).
    trades_sent: Option<Instant>,
}

impl QuoteBatch {
    /// Queue a quote's bar and trade; the bar it closes, to send now.
    pub fn add(&mut self, bar: Option<ChartAggregate>, trade: TradeTick) -> Option<ChartAggregate> {
        self.trades.insert(trade.symbol.clone(), trade);
        let bar = bar?;
        let time = bar.time;
        self.bars.insert(bar.symbol.clone(), bar).filter(|prev| prev.time != time)
    }

    /// The bars to send, and the trades when their interval is over.
    pub fn take(&mut self, now: Instant) -> (Vec<ChartAggregate>, Vec<TradeTick>) {
        let bars = self.bars.drain().map(|(_, b)| b).collect();
        let due = self.trades_sent.is_none_or(|t| now.duration_since(t) >= TRADE_FLUSH);
        if !due || self.trades.is_empty() {
            return (bars, Vec::new());
        }
        self.trades_sent = Some(now);
        (bars, self.trades.drain().map(|(_, t)| t).collect())
    }
}

/// The windows whose charts show `symbol` (chart owners are "<window label>:<pane>").
pub fn chart_windows(charts: &HashMap<String, String>, symbol: &str) -> BTreeSet<String> {
    charts.iter().filter(|(_, s)| s.as_str() == symbol).map(|(owner, _)| owner.split(':').next().unwrap_or(owner).to_string()).collect()
}

/// Send a bar to `windows`.
pub fn send_bar(app: &AppHandle, bar: &ChartAggregate, windows: BTreeSet<String>) {
    for label in windows {
        let _ = bar.emit_to(app, label.as_str());
    }
}

/// Send what the batch holds: each bar to the windows `windows_of` its symbol, the trades when due.
pub fn flush(app: &AppHandle, batch: &Mutex<QuoteBatch>, windows_of: impl Fn(&str) -> BTreeSet<String>) {
    let (bars, trades) = batch.locked().take(Instant::now());
    for bar in &bars {
        send_bar(app, bar, windows_of(&bar.symbol));
    }
    for tick in trades {
        let _ = tick.emit(app);
    }
}

/// Run `flush` every bar flush, for the app's life.
pub fn spawn_flusher(app: &AppHandle, flush: fn(&AppHandle)) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut every = tokio::time::interval(BAR_FLUSH);
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            every.tick().await;
            flush(&app);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quote(symbol: &str, time: f64, close: f64) -> (Option<ChartAggregate>, TradeTick) {
        let bar = ChartAggregate { symbol: symbol.into(), time, open: close, high: close, low: close, close, volume: 1.0, day: None };
        let trade = TradeTick {
            symbol: symbol.into(),
            price: close,
            change: None,
            change_percent: None,
            ext_change_percent: None,
            volume: None,
            source: None,
            trade_price: Some(close),
            trade_time: Some(time),
            trade_high: None,
            trade_low: None,
            day_high: None,
            day_low: None,
        };
        (Some(bar), trade)
    }

    // specs/performance.md REQ-PERF-003.A1, A2
    #[test]
    fn quotes_are_sent_newest_per_symbol_per_flush_closed_bars_at_once() {
        let mut b = QuoteBatch::default();
        let t0 = Instant::now();
        for (i, close) in [1.0, 2.0, 3.0].into_iter().enumerate() {
            let (bar, trade) = quote("US:AAPL", 60.0, close);
            assert!(b.add(bar, trade).is_none(), "quote {i} of the same minute closes nothing");
        }
        let (bar, trade) = quote("HK:00700", 60.0, 9.0);
        b.add(bar, trade);
        let (bars, trades) = b.take(t0);
        assert_eq!(bars.len(), 2);
        assert_eq!(bars.iter().find(|x| x.symbol == "US:AAPL").unwrap().close, 3.0);
        assert_eq!(trades.len(), 2);
        // A minute later: the bar it follows is sent at once, with its last values.
        let (bar, trade) = quote("US:AAPL", 60.0, 4.0);
        b.add(bar, trade);
        let (bar, trade) = quote("US:AAPL", 120.0, 5.0);
        let closed = b.add(bar, trade).expect("the minute's last bar");
        assert_eq!((closed.time, closed.close), (60.0, 4.0));
        // Trades wait for their interval; bars go every flush.
        let (bars, trades) = b.take(t0 + Duration::from_millis(16));
        assert_eq!((bars.len(), trades.len()), (1, 0));
        let (_, trades) = b.take(t0 + TRADE_FLUSH);
        assert_eq!(trades.len(), 1);
        assert_eq!(trades[0].price, 5.0);
    }

    // specs/performance.md REQ-PERF-003.A3
    #[test]
    fn a_bar_goes_to_the_windows_that_chart_its_symbol() {
        let charts: HashMap<String, String> = [("main:p1", "US:AAPL"), ("main:p2", "US:AAPL"), ("chart-2:p1", "US:AAPL"), ("chart-3:p1", "HK:00700")]
            .into_iter()
            .map(|(o, s)| (o.to_string(), s.to_string()))
            .collect();
        assert_eq!(chart_windows(&charts, "US:AAPL").into_iter().collect::<Vec<_>>(), vec!["chart-2".to_string(), "main".to_string()]);
        assert!(chart_windows(&charts, "US:MSFT").is_empty());
    }
}
