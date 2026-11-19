/*
 * OpenTrader's moomoo plugin: a data and broker plugin (plugins/PROTOCOL.md, Plugin API 2) between the app and the
 * user's moomoo OpenD. The app starts it with its address and token in the environment; `run_session` says hello,
 * then answers `data.search`, `data.symbolInfo` and `data.bars` from OpenD, streams `data.quote` for the symbols of
 * `data.subscribe`, and trades through `broker.*` (`trade/`), until the app closes the connection. Symbols are
 * whole: `US:AAPL`, `HK:00700`.
 */
pub mod bars;
pub mod cli;
pub mod history;
pub mod hours;
pub mod market;
pub mod opend;
// Shared with the other data plugins (plugins/kit).
pub use opentrader_plugin_kit::{pace, rpc, store};
pub mod search;
pub mod stream;
pub mod trade;

use bars::OpenD;
use history::{Ask, History};
use market::security_of;
use std::collections::BTreeSet;
use store::{Backend, Store, StoreSettings};
use opend::client::{Client, Event};
use opend::proto;
use opentrader_plugin_kit::session::{self, Handler};
use rpc::{params, Out, RpcError, BAD_PARAMS, UNKNOWN_METHOD};
use search::Catalog;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use stream::{Quotes, Subscriptions};
use tokio::sync::mpsc;

/// The plugin's id: its folder and tables.
pub const ID: &str = "moomoo";
/// How often dropped symbols are checked for their minute being up.
const RELEASE_EVERY: Duration = Duration::from_secs(10);
/// How often streamed bars are written to the store.
const FLUSH_EVERY: Duration = Duration::from_secs(30);
pub const DEFAULT_OPEND: &str = "127.0.0.1:11111";

struct Plugin {
    client: Client,
    history: History<OpenD>,
    store: Arc<Store>,
    /// The store's settings in use, to move only when they change.
    store_settings: StoreSettings,
    /// What the stream's closed minutes cover.
    minute_cover: Mutex<stream::MinuteCover>,
    catalog: Catalog,
    subs: tokio::sync::Mutex<Subscriptions>,
    /// The tickers of `data.subscribe` and of `broker.watchQuote`: both are subscribed at OpenD.
    data_tickers: Mutex<BTreeSet<String>>,
    quote_tickers: Mutex<BTreeSet<String>>,
    /// The "Always record" symbols: kept subscribed, so their closed minutes are stored with no chart open.
    record_tickers: Mutex<BTreeSet<String>>,
    broker: trade::Broker,
    quotes: Mutex<Quotes>,
    out: Out,
}

/// One session with the app: Ok when the app closed the connection, Err with the reason when it refused the plugin
/// or could not be reached.
pub async fn run_session(addr: &str, plugin: &str, token: &str) -> Result<(), String> {
    run_session_with(addr, plugin, token, DEFAULT_OPEND).await
}

/// `run_session`, with OpenD first looked for at `opend` (until `plugin.configure` gives the user's address).
pub async fn run_session_with(addr: &str, plugin: &str, token: &str, opend: &str) -> Result<(), String> {
    opentrader_plugin_kit::set_plugin(ID);
    let (out, lines) = session::connect(addr, plugin, token, env!("CARGO_PKG_VERSION")).await?;

    let (client, events) = Client::new(opend.to_string());
    tokio::spawn(client.clone().run());
    let store = Arc::new(Store::new(Backend::embedded()));
    let me = Arc::new(Plugin {
        history: History { source: OpenD::new(client.clone()), store: store.clone() },
        broker: trade::Broker::new(client.clone(), out.clone()),
        client,
        store,
        store_settings: StoreSettings::default(),
        minute_cover: Mutex::new(stream::MinuteCover::default()),
        catalog: Catalog::with_dir(store::cache_dir()),
        subs: tokio::sync::Mutex::new(Subscriptions::default()),
        data_tickers: Mutex::new(BTreeSet::new()),
        quote_tickers: Mutex::new(BTreeSet::new()),
        record_tickers: Mutex::new(BTreeSet::new()),
        quotes: Mutex::new(Quotes::default()),
        out: out.clone(),
    });
    tokio::spawn(me.clone().pump(events));
    tokio::spawn(me.clone().release());
    tokio::spawn(me.clone().flush_often());
    me.broker.send_state().await;
    let broker = me.clone();
    tokio::spawn(async move { broker.broker.refresh_often().await });

    // Requests run on their own (a long history doesn't hold a search up); notifications in order.
    session::serve(lines, out, me.clone()).await;
    me.store.flush().await;
    Ok(())
}

#[derive(Deserialize)]
struct SearchParams {
    query: String,
}

#[derive(Deserialize)]
struct TickerParams {
    ticker: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BarsParams {
    ticker: String,
    resolution: String,
    from: f64,
    to: f64,
    count_back: usize,
}

#[derive(Deserialize)]
struct ConfigureParams {
    settings: std::collections::HashMap<String, String>,
}

/// The "Always record" setting's symbols (separated by commas, spaces or new lines), and the entries refused.
pub fn record_list(text: &str) -> (BTreeSet<String>, Vec<String>) {
    opentrader_plugin_kit::feed::record_list_with(text, |e| security_of(e).and_then(|(_, sec)| market::symbol_of(&sec)))
}

fn locked<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Deserialize)]
struct SubscribeParams {
    tickers: Vec<String>,
}

fn not_served(ticker: &str) -> RpcError {
    RpcError::new(BAD_PARAMS, format!("{ticker} is not a symbol of this plugin (US:AAPL, HK:00700, SH:600519, SZ:000001)"))
}

impl Plugin {
    async fn request(&self, method: &str, p: Value) -> Result<Value, RpcError> {
        match method {
            "data.search" => {
                let p: SearchParams = params(method, p)?;
                Ok(json!({ "symbols": self.catalog.search(&self.client, &p.query).await }))
            }
            "data.symbolInfo" => {
                let p: TickerParams = params(method, p)?;
                self.symbol_info(&p.ticker).await
            }
            "data.bars" => {
                let p: BarsParams = params(method, p)?;
                let (market, sec) = security_of(&p.ticker).ok_or_else(|| not_served(&p.ticker))?;
                let symbol = market::symbol_of(&sec).unwrap_or_default();
                let ask = Ask { market, sec: &sec, symbol: &symbol, resolution: &p.resolution, from: p.from, to: p.to, count: p.count_back };
                let bars = self.history.bars(ask, chrono::Utc::now().timestamp()).await?;
                Ok(json!({ "bars": bars }))
            }
            _ => match self.broker.request(method, p).await {
                Some(answer) => answer,
                None => Err(RpcError::new(UNKNOWN_METHOD, format!("Unknown method {method}"))),
            },
        }
    }

    async fn symbol_info(&self, ticker: &str) -> Result<Value, RpcError> {
        let (market, sec) = security_of(ticker).ok_or_else(|| not_served(ticker))?;
        let symbol = market::symbol_of(&sec).unwrap_or_default();
        let found = match self.catalog.find(&symbol).await {
            Some(l) => Some(l),
            None => {
                let s2c = self.client.request(proto::QOT_STATIC_INFO, json!({ "securityList": [sec] })).await?;
                search::listed(&s2c).into_iter().next()
            }
        };
        let l = found.ok_or_else(|| RpcError::from(format!("OpenD knows no stock, fund or index {symbol}")))?;
        Ok(json!({
            "description": l.name, "type": l.kind, "currency": market.currency(),
            "timezone": market.tz().name(), "session": market.session(), "extended": market.extended(), "holidays": "",
            "pricescale": market.pricescale(), "minmov": 1, "intervals": bars::INTERVALS,
        }))
    }

    async fn notification(self: &Arc<Self>, method: &str, p: Value) {
        match method {
            "plugin.configure" => {
                let Ok(p) = serde_json::from_value::<ConfigureParams>(p) else { return };
                let host = p.settings.get("host").map(|h| h.trim()).filter(|h| !h.is_empty()).unwrap_or("127.0.0.1");
                let port = p.settings.get("port").and_then(|v| v.trim().parse::<u16>().ok()).unwrap_or(11111);
                self.client.set_addr(format!("{host}:{port}"));
                self.client.set_key(p.settings.get("rsaKeyFile").cloned());
                self.store_settings.apply(&self.store, &p.settings).await;
                let (record, refused) = record_list(p.settings.get("alwaysRecord").map_or("", |s| s));
                if !refused.is_empty() {
                    println!("[warn] Always record: {} left out (symbols are EXCHANGE:CODE, such as US:AAPL)", refused.join(", "));
                }
                let changed = {
                    let mut set = locked(&self.record_tickers);
                    let changed = *set != record;
                    *set = record;
                    changed
                };
                if changed {
                    let list: Vec<String> = locked(&self.record_tickers).iter().cloned().collect();
                    if !list.is_empty() {
                        println!("[info] always recording 1-minute bars of {}", list.join(", "));
                    }
                    self.resubscribe().await;
                }
            }
            "data.subscribe" | "broker.watchQuote" => {
                let Ok(p) = serde_json::from_value::<SubscribeParams>(p) else { return };
                let set = if method == "data.subscribe" { &self.data_tickers } else { &self.quote_tickers };
                *locked(set) = p.tickers.into_iter().collect();
                self.resubscribe().await;
            }
            _ => {}
        }
    }

    /// Subscribes what the app asks for and the "Always record" symbols; the others are released once OpenD allows.
    /// The set changes now, in the order of the notifications; OpenD is asked on its own task, so that a slow OpenD
    /// does not hold up the requests after them.
    async fn resubscribe(self: &Arc<Self>) {
        let mut wanted: BTreeSet<String> = locked(&self.data_tickers).union(&locked(&self.quote_tickers)).cloned().collect();
        wanted.extend(locked(&self.record_tickers).iter().cloned());
        let new = self.subs.lock().await.set(wanted, tokio::time::Instant::now());
        let me = self.clone();
        tokio::spawn(async move { me.subscribe(new).await });
    }

    async fn subscribe(&self, symbols: Vec<String>) {
        if symbols.is_empty() {
            return;
        }
        if let Err(e) = self.client.request(proto::QOT_SUB, stream::sub_request(&symbols, true)).await {
            println!("[warn] live prices of {} are off: {e}", symbols.join(", "));
            self.subs.lock().await.forget(&symbols);
        }
    }

    /// OpenD's events: a new connection subscribes the set again; pushes become quotes.
    async fn pump(self: Arc<Self>, mut events: mpsc::UnboundedReceiver<Event>) {
        while let Some(e) = events.recv().await {
            let now = chrono::Utc::now().timestamp();
            match e {
                Event::Connected => {
                    // Minutes missed while disconnected are not known: coverage starts again.
                    locked(&self.minute_cover).reset();
                    let again = self.subs.lock().await.reconnected(tokio::time::Instant::now());
                    let me = self.clone();
                    tokio::spawn(async move { me.subscribe(again).await });
                    let me = self.clone();
                    tokio::spawn(async move { me.broker.connected().await });
                    // The whole US list: loaded on the first run, then when a day old (search.rs).
                    let me = self.clone();
                    tokio::spawn(async move {
                        if let Err(e) = me.catalog.keep_fresh(&me.client, chrono::Utc::now().timestamp()).await {
                            println!("[warn] the US list could not be loaded: {e}");
                        }
                    });
                }
                Event::Push { proto: p @ (proto::TRD_UPDATE_ORDER | proto::TRD_UPDATE_ORDER_FILL), s2c } => {
                    let me = self.clone();
                    tokio::spawn(async move { me.broker.push(p, &s2c).await });
                }
                Event::Push { proto: proto::QOT_UPDATE_KL, s2c } => {
                    let (closed, sent) = {
                        let subs = self.subs.lock().await;
                        let mut quotes = self.quotes.lock().unwrap_or_else(|e| e.into_inner());
                        let closed = quotes.minute_push(&s2c, now);
                        // The chart moves on the minute's close too (REQ-MOOMOO-005.A4).
                        let symbol = s2c.get("security").and_then(|v| serde_json::from_value::<proto::Security>(v.clone()).ok()).and_then(|s| market::symbol_of(&s));
                        (closed, symbol.and_then(|sym| quotes.minute_quote(&sym, &subs, now)))
                    };
                    self.send_quotes(sent.into_iter().collect());
                    if let Some((symbol, bar)) = closed {
                        stream::store_minute(&self.store, &self.minute_cover, &symbol, bar).await;
                    }
                }
                Event::Push { proto: proto::QOT_UPDATE_BASIC, s2c } => {
                    let sent = {
                        let subs = self.subs.lock().await;
                        self.quotes.lock().unwrap_or_else(|e| e.into_inner()).basic_push(&s2c, &subs, now)
                    };
                    self.send_quotes(sent);
                }
                Event::Push { .. } => {}
            }
        }
    }

    /// Unsubscribes dropped symbols once OpenD allows it.
    async fn release(self: Arc<Self>) {
        let mut tick = tokio::time::interval(RELEASE_EVERY);
        loop {
            tick.tick().await;
            let due = self.subs.lock().await.due(tokio::time::Instant::now());
            if !due.is_empty() {
                let mut cover = locked(&self.minute_cover);
                due.iter().for_each(|s| cover.forget(s));
            }
            if !due.is_empty() {
                if let Err(e) = self.client.request(proto::QOT_SUB, stream::sub_request(&due, false)).await {
                    println!("[warn] could not stop live prices of {}: {e}", due.join(", "));
                }
            }
        }
    }

    /// Live quotes to the app: `data.quote` for the chart's symbols, `broker.quote` for the order ticket's.
    fn send_quotes(&self, sent: Vec<Value>) {
        if sent.is_empty() {
            return;
        }
        let (data, watched) = (locked(&self.data_tickers).clone(), locked(&self.quote_tickers).clone());
        for q in sent {
            let ticker = q["ticker"].as_str().unwrap_or_default();
            if watched.contains(ticker) {
                // Basic quotes carry no bid and ask: the ticket shows the last price.
                self.out.notify("broker.quote", json!({ "ticker": ticker, "bid": null, "ask": null, "last": q["price"] }));
            }
            if data.contains(ticker) {
                self.out.notify("data.quote", q);
            }
        }
    }

    async fn flush_often(self: Arc<Self>) {
        let mut tick = tokio::time::interval(FLUSH_EVERY);
        loop {
            tick.tick().await;
            self.store.flush().await;
        }
    }
}

impl Handler for Plugin {
    async fn request(self: Arc<Self>, method: String, params: Value) -> Result<Value, RpcError> {
        Plugin::request(&self, &method, params).await
    }

    async fn notification(self: Arc<Self>, method: String, params: Value) {
        Plugin::notification(&self, &method, params).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn always_record_folds_case_and_duplicates_and_leaves_out_others() {
        let (ok, refused) = record_list(" us:aapl, US:AAPL\nUS:msft;XX:NOPE  AAPL ");
        assert_eq!(ok.into_iter().collect::<Vec<_>>(), ["US:AAPL", "US:MSFT"]);
        assert_eq!(refused, ["XX:NOPE", "AAPL"]);
        assert!(record_list("").0.is_empty());
    }
}
