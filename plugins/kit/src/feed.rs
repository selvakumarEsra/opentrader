/*
 * A whole data plugin over a broker (`Provider`): the broker gives its instrument list, history requests and a
 * stream of ticks; this answers the app (plugins/PROTOCOL.md, data plugins):
 *   data.search, data.symbolInfo   the instrument list (catalog.rs), kept on disk and loaded again daily
 *   data.bars                      the bar store, the broker asked only for the missing trading time (history.rs)
 *   data.subscribe                 the stream of the wanted symbols and the "Always record" ones; each tick a
 *                                  data.quote, each whole minute written to the store (live.rs)
 *   plugin.configure               the broker's credentials, the store (ClickHouse or the embedded SQLite file)
 *   data.screen                    a screener market's rows (screen.rs): the broker's quotes of the market's
 *                                  instruments every 30 s, and values from the daily bars, filled in the background
 */
use crate::calendar::Calendar;
use crate::catalog::{Catalog, Instrument};
use crate::history::{History, Source, Span};
use crate::live::{store_minute, Minutes, Tick};
use crate::rpc::{params, Out, RpcError, BAD_PARAMS, UNKNOWN_METHOD};
use crate::screen::{self, Quote};
use crate::store::Key;
use crate::session::Handler;
use crate::store::{Backend, Store, StoreSettings};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch};

/// How often a screener market's quotes are asked for again.
const SCREEN_REFRESH: Duration = Duration::from_secs(30);
/// How far back a screener market's daily bars go: a year of sessions and the 200-day average.
const SCREEN_DAYS: i64 = 400;
/// How often a screener market's daily bars are checked for missing sessions once all are filled.
const SCREEN_HISTORY_EVERY: Duration = Duration::from_secs(15 * 60);

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
    /// The currency of the broker's prices.
    const CURRENCY: &'static str;

    /// The trading week of `exchange` (as in `EXCHANGE:SYMBOL`): which gaps may hold bars, where a day starts.
    fn calendar(exchange: &str) -> Calendar;

    /// The user's settings (credentials), at start and whenever they change.
    fn configure(&self, settings: &HashMap<String, String>) -> impl Future<Output = ()> + Send;

    /// The broker's whole instrument list.
    fn instruments(&self) -> impl Future<Output = Result<Vec<Instrument>, String>> + Send;

    /// Streams the instruments of `wanted` (following its changes) for the plugin's life: `Event::Connected` after
    /// each connection, then their ticks. Reconnects on its own.
    fn stream(self: Arc<Self>, wanted: watch::Receiver<Vec<Instrument>>, events: mpsc::UnboundedSender<Event>) -> impl Future<Output = ()> + Send;

    /// The screener markets the plugin serves (`data.screen`, plugins/PROTOCOL.md, data.screen), such as `["IN"]`. None by default.
    const SCREEN_MARKETS: &'static [&'static str] = &[];
    /// A market's exchanges in the order a company's listing on them counts as its primary one.
    const SCREEN_PRIMARY: &'static [&'static str] = &[];
    /// The most instruments one `quotes` call takes.
    const QUOTE_BATCH: usize = 500;

    /// Whether `inst` is one of the rows of screener market `market`.
    fn screen_member(_market: &str, _inst: &Instrument) -> bool {
        false
    }

    /// The broker's quotes of `insts` (at most `QUOTE_BATCH`), by symbol, within its rate limit.
    fn quotes(&self, _insts: &[Instrument]) -> impl Future<Output = Result<HashMap<String, Quote>, String>> + Send {
        async { Err("this plugin gives no quotes for a screener".to_string()) }
    }
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
    store_settings: StoreSettings,
    catalog: Catalog,
    /// `data.subscribe`'s symbols, and the "Always record" ones.
    data: Mutex<BTreeSet<String>>,
    record: Mutex<BTreeSet<String>>,
    wanted: watch::Sender<Vec<Instrument>>,
    minutes: Mutex<Minutes>,
    out: Out,
    /// Per screener market: its rows and their refresh.
    screens: HashMap<String, screen::State>,
}

fn locked<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

use crate::now;
pub use crate::store::backend_of;

/// The "Always record" setting's symbols (`NSE:INFY, NFO:NIFTY26NOVFUT`, separated by commas, spaces or new lines)
/// and the entries that are not `EXCHANGE:SYMBOL`.
pub fn record_list(text: &str) -> (BTreeSet<String>, Vec<String>) {
    record_list_with(text, |e| match e.split_once(':') {
        Some((x, t)) if !x.is_empty() && !t.is_empty() => Some(e.to_ascii_uppercase()),
        _ => None,
    })
}

/// `record_list`, each entry made the plugin's symbol by `symbol` (None: not one).
pub fn record_list_with(text: &str, symbol: impl Fn(&str) -> Option<String>) -> (BTreeSet<String>, Vec<String>) {
    let mut ok = BTreeSet::new();
    let mut refused = Vec::new();
    for e in text.split(|c: char| c == ',' || c == ';' || c.is_whitespace()).filter(|e| !e.is_empty()) {
        match symbol(e) {
            Some(s) => {
                ok.insert(s);
            }
            None => refused.push(e.to_string()),
        }
    }
    (ok, refused)
}

#[derive(Deserialize)]
struct SearchParams {
    query: String,
}

#[derive(Deserialize)]
struct ScreenParams {
    market: String,
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
                let cal = P::calendar(i.exchange());
                Ok(json!({
                    "description": i.name, "type": i.kind, "currency": P::CURRENCY, "timezone": cal.timezone(),
                    "session": cal.session, "extended": null, "holidays": "", "pricescale": i.pricescale(), "minmov": 1,
                    "intervals": P::INTERVALS,
                }))
            }
            "data.bars" => {
                let p: BarsParams = params(method, p)?;
                let i = self.instrument(&p.ticker).await?;
                let cal = P::calendar(i.exchange());
                let bars = self.history.bars(&i, &cal, &p.resolution, Span { from: p.from as i64, to: p.to as i64, count: p.count_back }, now()).await?;
                Ok(json!({ "bars": bars }))
            }
            _ => Err(RpcError::new(UNKNOWN_METHOD, format!("Unknown method {method}"))),
        }
    }

    async fn notification(self: Arc<Self>, method: &str, p: Value) {
        match method {
            "plugin.configure" => {
                let Ok(p) = serde_json::from_value::<ConfigureParams>(p) else {
                    return;
                };
                self.store_settings.apply(&self.store, &p.settings).await;
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
                let Ok(p) = serde_json::from_value::<SubscribeParams>(p) else {
                    return;
                };
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
                    let cal = P::calendar(t.symbol.split_once(':').map_or("", |(x, _)| x));
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
}

/// Screener markets (`data.screen`).
impl<P: Provider> Feed<P> {
    /// The market's rows as last built; the first ask (or one after the app stopped asking) starts their refresh.
    fn screen(self: Arc<Self>, p: Value) -> Result<Value, RpcError> {
        let p: ScreenParams = params("data.screen", p)?;
        let state = self.screens.get(&p.market).ok_or_else(|| RpcError::new(BAD_PARAMS, format!("No screener market {}", p.market)))?;
        let (mut answer, start) = state.ask();
        if start {
            tokio::spawn(self.clone().screen_loop(p.market.clone()));
        }
        if answer.market.is_empty() {
            answer.market = p.market;
            answer.currency = P::CURRENCY.into();
            answer.status = Some("Loading the market's quotes".into());
        }
        Ok(serde_json::to_value(answer).unwrap_or(Value::Null))
    }

    fn screen_members(&self, market: &str) -> Vec<Instrument> {
        self.catalog.select(|i| P::screen_member(market, i))
    }

    /// Keeps `market`'s rows current while the app asks for them: quotes every `SCREEN_REFRESH`, values from the
    /// daily bars (filled by `screen_history` meanwhile).
    async fn screen_loop(self: Arc<Self>, market: String) {
        let Some(state) = self.screens.get(&market) else { return };
        if self.catalog.is_empty() {
            self.fresh_list().await;
        }
        let history = tokio::spawn(self.clone().screen_history(market.clone()));
        let mut quotes: HashMap<String, Quote> = HashMap::new();
        while state.wanted() {
            let started = Instant::now();
            let members = self.screen_members(&market);
            let mut error = None;
            for chunk in members.chunks(P::QUOTE_BATCH.max(1)) {
                match self.provider.quotes(chunk).await {
                    Ok(q) => quotes.extend(q),
                    Err(e) => {
                        error = Some(e);
                        break;
                    }
                }
            }
            let rows = self.screen_rows(&members, &quotes).await;
            state.update(|a| {
                a.market = market.clone();
                a.currency = P::CURRENCY.into();
                a.rows = rows;
                a.fields = screen::TECHNICAL_FIELDS.iter().map(|f| f.to_string()).collect();
                a.updated = Some(chrono::Utc::now().timestamp_millis() as f64);
                a.error = error;
                if a.status.as_deref() == Some("Loading the market's quotes") {
                    a.status = None;
                }
            });
            if let Some(rest) = SCREEN_REFRESH.checked_sub(started.elapsed()) {
                tokio::time::sleep(rest).await;
            }
        }
        history.abort();
    }

    /// The rows of `members`: their quotes, and the values of their daily bars with today's session.
    async fn screen_rows(&self, members: &[Instrument], quotes: &HashMap<String, Quote>) -> Vec<screen::Row> {
        let kinds = screen::listing_kinds(members.iter().map(|i| i.symbol.as_str()), P::SCREEN_PRIMARY);
        let t = now();
        let mut rows = Vec::with_capacity(members.len());
        for (inst, kind) in members.iter().zip(kinds) {
            let cal = P::calendar(inst.exchange());
            let today = cal.midnight(t);
            let daily = self.store.read(&Key::new(&inst.symbol, "1D", false), |s| s.window(t - SCREEN_DAYS * 86_400, t)).await;
            let q = quotes.get(&inst.symbol);
            let values = screen::technicals(&screen::session_bars(daily, q, today), cal.tz);
            rows.push(screen::row(&inst.symbol, &inst.name, kind, q, today, values));
        }
        rows
    }

    /// Fills the daily bars of `market`'s instruments, the primary exchange's first, within the broker's history
    /// limit; then checks again every `SCREEN_HISTORY_EVERY` for the sessions since. The status counts the progress.
    async fn screen_history(self: Arc<Self>, market: String) {
        let Some(state) = self.screens.get(&market) else { return };
        let rank = |e: &str| P::SCREEN_PRIMARY.iter().position(|x| *x == e).unwrap_or(P::SCREEN_PRIMARY.len());
        loop {
            let mut members = self.screen_members(&market);
            members.sort_by_key(|i| rank(i.exchange()));
            let total = members.len();
            let mut done = 0;
            for inst in &members {
                if !state.wanted() {
                    return;
                }
                let cal = P::calendar(inst.exchange());
                let t = now();
                let span = Span { from: t - SCREEN_DAYS * 86_400, to: t, count: 0 };
                match self.history.fill(inst, &cal, "1D", span, t).await {
                    Ok(None) => done += 1,
                    Ok(Some(e)) | Err(e) => {
                        state.update(|a| a.status = Some(format!("Daily history: {done} of {total} stocks ({e})")));
                        tokio::time::sleep(Duration::from_secs(60)).await;
                        break;
                    }
                }
                if done % 25 == 0 || done == total {
                    state.update(|a| a.status = (done < total).then(|| format!("Daily history: {done} of {total} stocks")));
                }
            }
            if done == total {
                state.update(|a| a.status = None);
                let until = Instant::now() + SCREEN_HISTORY_EVERY;
                while Instant::now() < until {
                    if !state.wanted() {
                        return;
                    }
                    tokio::time::sleep(Duration::from_secs(10)).await;
                }
            }
        }
    }
}

impl<P: Provider> Handler for Feed<P> {
    async fn request(self: Arc<Self>, method: String, params: Value) -> Result<Value, RpcError> {
        if method == "data.screen" {
            return self.screen(params);
        }
        Feed::request(&self, &method, params).await
    }

    async fn notification(self: Arc<Self>, method: String, params: Value) {
        Feed::notification(self, &method, params).await
    }
}

/// One session with the app at `addr`, serving `provider`, until the app closes it.
pub async fn run<P: Provider>(addr: &str, plugin: &str, token: &str, provider: P) -> Result<(), String> {
    crate::set_plugin(P::ID);
    let (out, lines) = crate::session::connect(addr, plugin, token, env!("CARGO_PKG_VERSION")).await?;
    let provider = Arc::new(provider);
    let store = Arc::new(Store::new(Backend::embedded()));
    let (wanted, wanted_rx) = watch::channel(Vec::new());
    let feed = Arc::new(Feed {
        history: History { source: provider.clone(), store: store.clone() },
        provider: provider.clone(),
        store,
        store_settings: StoreSettings::default(),
        catalog: Catalog::open(crate::store::cache_dir().map(|d| d.join("instruments.json"))),
        data: Mutex::new(BTreeSet::new()),
        record: Mutex::new(BTreeSet::new()),
        wanted,
        minutes: Mutex::new(Minutes::default()),
        out: out.clone(),
        screens: P::SCREEN_MARKETS.iter().map(|m| (m.to_string(), screen::State::default())).collect(),
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

    crate::session::serve(lines, out, feed.clone()).await;
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
