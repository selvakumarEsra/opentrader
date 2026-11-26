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
 *   token    a broker's access token: kept in memory and on disk, made again when refused
 *   socket   a broker's WebSocket stream of ticks, connected again after a drop (feature `socket`)
 *   backfill the plugin program as an unattended command filling the store
 *   screen   a screener market's rows: quotes and the values of daily bars (`data.screen`)
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
pub mod screen;
pub mod session;
#[cfg(feature = "socket")]
pub mod socket;
pub mod store;
pub mod token;
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

/// The plugin's name, as `set_plugin` gave it. Every plugin names itself first: without a name the store would not
/// know its folder or tables.
pub fn plugin() -> &'static str {
    PLUGIN.get().copied().expect("opentrader_plugin_kit::set_plugin must be called before the store is used")
}

/// Now, in UNIX seconds.
pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
}
