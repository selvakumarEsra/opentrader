/*
 * What OpenTrader's data plugins share:
 *   store    the OHLCV bar store (SQLite file, or ClickHouse), per series with the ranges it is sure of
 *   rpc      JSON-RPC answers and notifications to the app
 *   pace     a request pace (a broker's rate limit)
 *   session  the connection to the app: hello, requests in, answers out
 *   catalog  an instrument list kept on disk, searched and named
 *   calendar exchange sessions, for which gaps may hold bars
 *   history  `data.bars` from the store, asking the broker only for what is missing
 *   live     ticks into quotes and 1-minute bars written to the store
 *   feed     a whole data plugin over a broker (`Provider`)
 *   totp     RFC 6238 codes for broker logins
 */
pub mod backfill;
pub mod calendar;
pub mod catalog;
pub mod feed;
pub mod history;
pub mod live;
pub mod pace;
pub mod rpc;
pub mod session;
pub mod store;
pub mod totp;

use std::sync::OnceLock;

/// `[time, open, high, low, close, volume]`, time the bar's start in UNIX seconds.
pub type Bar = [f64; 6];

static PLUGIN: OnceLock<&'static str> = OnceLock::new();

/// Names the plugin once, at start: its cache folder (`opentrader-<name>`, `OPENTRADER_<NAME>_CACHE`) and its
/// ClickHouse tables (`<name>_bars`, `<name>_covered`).
pub fn set_plugin(name: &'static str) {
    let _ = PLUGIN.set(name);
}

/// The plugin's name; `moomoo` until `set_plugin`.
pub fn plugin() -> &'static str {
    PLUGIN.get().copied().unwrap_or("moomoo")
}
