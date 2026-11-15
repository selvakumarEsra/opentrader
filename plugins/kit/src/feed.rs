/*
 * A whole data plugin over a broker (`Provider`): the broker gives its instrument list, history requests and a
 * stream of ticks; this answers the app (plugins/PROTOCOL.md, data plugins):
 *   data.search, data.symbolInfo   the instrument list (catalog.rs), kept on disk and loaded again daily
 *   data.bars                      the bar store, the broker asked only for the missing trading time (history.rs)
 *   data.subscribe                 the stream of the wanted symbols and the "Always record" ones; each tick a
 *                                  data.quote, each whole minute written to the store (live.rs)
 *   plugin.configure               the broker's credentials, the store (ClickHouse or the embedded SQLite file)
 */
use crate::calendar::Calendar;
use crate::catalog::{Catalog, Instrument};
use crate::history::{History, Source, Span};
use crate::live::{store_minute, Minutes, Tick};
use crate::rpc::{params, Out, RpcError, BAD_PARAMS, PARSE_ERROR, UNKNOWN_METHOD};
use crate::store::{clickhouse::ClickHouse, Backend, Store};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, watch};

const FLUSH_EVERY: Duration = Duration::from_secs(30);
/// How often the instrument list is checked for being a day old.
const LIST_CHECK_EVERY: Duration = Duration::from_secs(3600);

/// What the stream tells.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// (Re)connected: minutes in progress missed ticks.
    Connected,
    Tick(Tick),
}

pub trait Provider: Source + Send + Sync + 'static {
    /// The plugin's id: its folder and tables (`crate::set_plugin`).
    const ID: &'static str;
    /// The resolutions `data.symbolInfo` offers.
    const INTERVALS: &'static [&'static str];

    /// The user's settings (credentials), at start and whenever they change.
    fn configure(&self, settings: &HashMap<String, String>) -> impl Future<Output = ()> + Send;

    /// The broker's whole instrument list.
    fn instruments(&self) -> impl Future<Output = Result<Vec<Instrument>, String>> + Send;

    /// Streams the instruments of `wanted` (following its changes) for the plugin's life: `Event::Connected` after
    /// each connection, then their ticks. Reconnects on its own.
    fn stream(self: Arc<Self>, wanted: watch::Receiver<Vec<Instrument>>, events: mpsc::UnboundedSender<Event>) -> impl Future<Output = ()> + Send;
}

/// Lets `History` hold the provider shared with the plugin.
impl<S: Source> Source for Arc<S> {
    fn fetch(&self, inst: &Instrument, resolution: &str, from: i64, to: i64) -> impl Future<Output = Result<Vec<crate::Bar>, String>> + Send {
        (**self).fetch(inst, resolution, from, to)
    }
    fn step(&self, resolution: &str) -> Option<i64> {
        (**self).step(resolution)
    }
    fn oldest(&self, resolution: &str, now: i64) -> Option<i64> {
        (**self).oldest(resolution, now)
    }
}

struct Feed<P: Provider> {
    provider: Arc<P>,
    history: History<Arc<P>>,
    store: Arc<Store>,
    store_settings: tokio::sync::Mutex<Option<String>>,
    catalog: Catalog,
    /// `data.subscribe`'s symbols, and the "Always record" ones.
    data: Mutex<BTreeSet<String>>,
    record: Mutex<BTreeSet<String>>,
    wanted: watch::Sender<Vec<Instrument>>,
    minutes: Mutex<Minutes>,
    out: Out,
}

fn locked<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// The "Always record" setting's symbols (`NSE:INFY, NFO:NIFTY26NOVFUT`, separated by commas, spaces or new lines)
/// and the entries that are not symbols.
pub fn record_list(text: &str) -> (BTreeSet<String>, Vec<String>) {
    let mut ok = BTreeSet::new();
    let mut refused = Vec::new();
    for e in text.split(|c: char| c == ',' || c == ';' || c.is_whitespace()).filter(|e| !e.is_empty()) {
        match e.split_once(':') {
            Some((x, t)) if !x.is_empty() && !t.is_empty() => {
                ok.insert(e.to_ascii_uppercase());
            }
            _ => refused.push(e.to_string()),
        }
    }
    (ok, refused)
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
    #[serde(default)]
    count_back: usize,
}

#[derive(Deserialize)]
struct ConfigureParams {
    settings: HashMap<String, String>,
}

#[derive(Deserialize)]
struct SubscribeParams {
    tickers: Vec<String>,
}

impl<P: Provider> Feed<P> {
    /// The instrument list, loaded now when there is none (first run) or it is a day old.
    async fn fresh_list(&self) {
        let p = self.provider.clone();
        match self.catalog.keep_fresh(now(), || async move { p.instruments().await }).await {
            Ok(true) => self.resubscribe(),
            Ok(false) => {}
            Err(e) => println!("[warn] the instrument list could not be loaded: {e}"),
        }
    }

    async fn instrument(&self, symbol: &str) -> Result<Instrument, RpcError> {
        if self.catalog.is_empty() {
            self.fresh_list().await;
        }
        self.catalog.find(symbol).ok_or_else(|| RpcError::new(BAD_PARAMS, format!("No instrument {symbol}")))
    }

    async fn request(&self, method: &str, p: Value) -> Result<Value, RpcError> {
        match method {
            "data.search" => {
                let p: SearchParams = params(method, p)?;
                if self.catalog.is_empty() {
                    self.fresh_list().await;
                }
                Ok(json!({ "symbols": self.catalog.search(&p.query) }))
            }
            "data.symbolInfo" => {
                let p: TickerParams = params(method, p)?;
                let i = self.instrument(&p.ticker).await?;
                let cal = Calendar::india(i.exchange());
                Ok(json!({
                    "description": i.name, "type": i.kind, "currency": "INR", "timezone": cal.timezone(),
                    "session": cal.session, "extended": null, "holidays": "", "pricescale": i.pricescale(), "minmov": 1,
                    "intervals": P::INTERVALS,
                }))
            }
            "data.bars" => {
                let p: BarsParams = params(method, p)?;
                let i = self.instrument(&p.ticker).await?;
                let cal = Calendar::india(i.exchange());
                let bars = self.history.bars(&i, &cal, &p.resolution, Span { from: p.from as i64, to: p.to as i64, count: p.count_back }, now()).await?;
                Ok(json!({ "bars": bars }))
            }
            _ => Err(RpcError::new(UNKNOWN_METHOD, format!("Unknown method {method}"))),
        }
    }

    async fn notification(self: Arc<Self>, method: &str, p: Value) {
        match method {
            "plugin.configure" => {
                let Ok(p) = serde_json::from_value::<ConfigureParams>(p) else { return };
                self.configure_store(&p.settings).await;
                let (record, refused) = record_list(p.settings.get("alwaysRecord").map_or("", |s| s));
                if !refused.is_empty() {
                    println!("[warn] Always record: {} left out (symbols are EXCHANGE:SYMBOL, such as NSE:INFY)", refused.join(", "));
                }
                *locked(&self.record) = record;
                self.provider.configure(&p.settings).await;
                self.resubscribe();
                // Credentials may be new: the list loads (or not) now rather than at the first search.
                tokio::spawn(async move { self.fresh_list().await });
            }
            "data.subscribe" => {
                let Ok(p) = serde_json::from_value::<SubscribeParams>(p) else { return };
                *locked(&self.data) = p.tickers.into_iter().map(|t| t.trim().to_ascii_uppercase()).collect();
                self.resubscribe();
            }
            _ => {}
        }
    }

    /// The stream's set: the app's symbols and the "Always record" ones, those of the list.
    fn resubscribe(&self) {
        let symbols: BTreeSet<String> = locked(&self.data).union(&locked(&self.record)).cloned().collect();
        let wanted: Vec<Instrument> = symbols.iter().filter_map(|s| self.catalog.find(s)).collect();
        self.wanted.send_if_modified(|w| {
            let changed = *w != wanted;
            *w = wanted;
            changed
        });
    }

    async fn on_events(self: Arc<Self>, mut events: mpsc::UnboundedReceiver<Event>) {
        while let Some(e) = events.recv().await {
            match e {
                Event::Connected => locked(&self.minutes).reset(),
                Event::Tick(t) => {
                    let cal = Calendar::india(t.symbol.split_once(':').map_or("", |(x, _)| x));
                    let (closed, quote) = locked(&self.minutes).tick(&t, &cal);
                    if let Some(bar) = closed {
                        store_minute(&self.store, &t.symbol, bar).await;
                    }
                    if locked(&self.data).contains(&t.symbol) {
                        self.out.notify("data.quote", quote);
                    }
                }
            }
        }
    }

    /// Settings > Plugins: ClickHouse when its URL is given, else the embedded store.
    async fn configure_store(&self, settings: &HashMap<String, String>) {
        let get = |k: &str| settings.get(k).map(|v| v.trim().to_string()).unwrap_or_default();
        let url = get("clickhouseUrl");
        let wanted = format!("{url}|{}|{}|{}", get("clickhouseDatabase"), get("clickhouseUser"), get("clickhousePassword"));
        let mut current = self.store_settings.lock().await;
        if current.as_deref() == Some(wanted.as_str()) {
            return;
        }
        let backend = if url.is_empty() {
            None
        } else {
            let db = Some(get("clickhouseDatabase")).filter(|d| !d.is_empty()).unwrap_or_else(|| "trading_db".into());
            let user = Some(get("clickhouseUser")).filter(|u| !u.is_empty()).unwrap_or_else(|| "default".into());
            match ClickHouse::new(&url, &db, &user, settings.get("clickhousePassword").map_or("", |s| s)) {
                Ok(c) => Some(Backend::ClickHouse(c)),
                Err(e) => {
                    println!("[warn] {e}; bars stay in the embedded store");
                    None
                }
            }
        };
        if backend.is_some() || current.is_some() {
            self.store.set_backend(backend.unwrap_or_else(Backend::embedded)).await;
        }
        *current = Some(wanted);
    }
}

/// One session with the app at `addr`, serving `provider`, until the app closes it.
pub async fn run<P: Provider>(addr: &str, plugin: &str, token: &str, provider: P) -> Result<(), String> {
    crate::set_plugin(P::ID);
    let (out, mut lines) = crate::session::connect(addr, plugin, token, env!("CARGO_PKG_VERSION")).await?;
    let provider = Arc::new(provider);
    let store = Arc::new(Store::new(Backend::embedded()));
    let (wanted, wanted_rx) = watch::channel(Vec::new());
    let feed = Arc::new(Feed {
        history: History { source: provider.clone(), store: store.clone() },
        provider: provider.clone(),
        store,
        store_settings: tokio::sync::Mutex::new(None),
        catalog: Catalog::open(crate::store::cache_dir().map(|d| d.join("instruments.json"))),
        data: Mutex::new(BTreeSet::new()),
        record: Mutex::new(BTreeSet::new()),
        wanted,
        minutes: Mutex::new(Minutes::default()),
        out: out.clone(),
    });
    let (events, events_rx) = mpsc::unbounded_channel();
    tokio::spawn(provider.stream(wanted_rx, events));
    tokio::spawn(feed.clone().on_events(events_rx));
    let f = feed.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(FLUSH_EVERY);
        loop {
            tick.tick().await;
            f.store.flush().await;
        }
    });
    let f = feed.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(LIST_CHECK_EVERY);
        tick.tick().await;
        loop {
            tick.tick().await;
            f.fresh_list().await;
        }
    });

    while let Ok(Some(line)) = lines.next_line().await {
        let msg: Value = match serde_json::from_str(&line) {
            Ok(m) => m,
            Err(e) => {
                out.answer(Value::Null, Err(RpcError::new(PARSE_ERROR, format!("Parse error: {e}"))));
                continue;
            }
        };
        let Some(method) = msg.get("method").and_then(Value::as_str).map(str::to_string) else { continue };
        let p = msg.get("params").cloned().unwrap_or(Value::Null);
        let me = feed.clone();
        match msg.get("id").cloned() {
            Some(id) => {
                tokio::spawn(async move { me.out.answer(id, me.request(&method, p).await) });
            }
            None => {
                tokio::spawn(async move { me.notification(&method, p).await });
            }
        }
    }
    feed.store.flush().await;
    Ok(())
}

/// The plugin program: one session with the app that started it, then exit (the app starts it again).
pub fn main<P: Provider>(provider: P) {
    crate::set_plugin(P::ID);
    let (addr, plugin, token) = crate::session::from_env(P::ID);
    let runtime = tokio::runtime::Runtime::new().unwrap_or_else(|e| {
        eprintln!("Cannot start: {e}");
        std::process::exit(1);
    });
    match runtime.block_on(run(&addr, &plugin, &token, provider)) {
        Ok(()) => {
            println!("[info] OpenTrader closed the connection");
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
