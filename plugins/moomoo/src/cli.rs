/*
 * The plugin program's own commands, run by the user outside the app, writing to the same bar store the app's
 * charts read:
 *
 *   opentrader-moomoo backfill [options] SYMBOL… | --file LIST   every bar OpenD has that the store lacks
 *   opentrader-moomoo record   [options] SYMBOL… | --file LIST   live 1-minute bars, until Ctrl+C
 *
 * Daily bars come in one request. 1-minute bars come in 30-day steps from now back, until a step holds none (or
 * `--from`). What the store covers is not asked again, so a later run fetches only the new bars.
 */
use crate::bars::{self, kl_type};
use crate::history::{Ask, History, Source};
use crate::market::security_of;
use crate::opend::client::{Client, Event};
use crate::opend::proto;
use crate::pace::Pace;
use crate::store::{clickhouse::ClickHouse, Backend, Store};
use crate::stream::{self, MinuteCover, Quotes};
use chrono::{NaiveDate, TimeZone, Utc};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const USAGE: &str = "\
Usage:
  opentrader-moomoo backfill [options] SYMBOL...   store every bar OpenD has that the store lacks
  opentrader-moomoo record   [options] SYMBOL...   store live 1-minute bars until Ctrl+C

Symbols: US:AAPL HK:00700 SH:600519 SZ:000001, or --file LIST (one or more per line, # comments).

Options:
  --opend HOST:PORT     OpenD's API address (127.0.0.1:11111)
  --key FILE            OpenD's RSA private key file, when its encryption is on
  --resolution LIST     backfill: bar sizes, from 1 3 5 15 30 60 1D (1,1D)
  --from DATE           backfill: the oldest day, dd/mm/yyyy or yyyy-mm-dd (as far back as OpenD has)
  --clickhouse URL      store in ClickHouse (http://localhost:8123) instead of the embedded store
  --database NAME       ClickHouse database (trading_db)
  --user NAME           ClickHouse user (default)
  --password TEXT       ClickHouse password (or OPENTRADER_MOOMOO_CLICKHOUSE_PASSWORD)
";

/// One step of a 1-minute backfill.
const STEP: i64 = 30 * 86_400;
/// No backfill reaches further back than this.
const OLDEST: &str = "1990-01-01";
const CONNECT_WAIT: Duration = Duration::from_secs(30);
const FLUSH_EVERY: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Backfill,
    Record,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Options {
    pub command: Command,
    pub opend: String,
    pub key: Option<String>,
    pub resolutions: Vec<String>,
    pub from: Option<i64>,
    pub clickhouse: Option<String>,
    pub database: String,
    pub user: String,
    pub password: String,
    pub symbols: Vec<String>,
}

/// A day as UNIX seconds at UTC midnight: dd/mm/yyyy or yyyy-mm-dd.
fn day_of(s: &str) -> Option<i64> {
    let d = NaiveDate::parse_from_str(s, "%d/%m/%Y").or_else(|_| NaiveDate::parse_from_str(s, "%Y-%m-%d")).ok()?;
    Some(Utc.from_utc_datetime(&d.and_hms_opt(0, 0, 0)?).timestamp())
}

/// dd/mm/yyyy of UNIX seconds, in UTC.
fn shown(t: i64) -> String {
    Utc.timestamp_opt(t, 0).single().map_or_else(|| t.to_string(), |d| d.format("%d/%m/%Y").to_string())
}

fn symbols_in(text: &str) -> Vec<String> {
    text.lines()
        .map(|l| l.split('#').next().unwrap_or(""))
        .flat_map(|l| l.split([',', ' ', '\t']))
        .map(|s| s.trim().to_ascii_uppercase())
        .filter(|s| !s.is_empty())
        .collect()
}

/// The command line after the program's name; Err is the message to show (with the usage when it is a mistake).
pub fn parse(args: &[String]) -> Result<Options, String> {
    let mut it = args.iter();
    let command = match it.next().map(String::as_str) {
        Some("backfill") => Command::Backfill,
        Some("record") => Command::Record,
        _ => return Err(USAGE.to_string()),
    };
    let mut o = Options {
        command,
        opend: crate::DEFAULT_OPEND.into(),
        key: None,
        resolutions: vec!["1".into(), "1D".into()],
        from: None,
        clickhouse: None,
        database: "trading_db".into(),
        user: "default".into(),
        password: std::env::var("OPENTRADER_MOOMOO_CLICKHOUSE_PASSWORD").unwrap_or_default(),
        symbols: Vec::new(),
    };
    while let Some(arg) = it.next() {
        let mut value = || it.next().cloned().ok_or_else(|| format!("{arg} needs a value\n\n{USAGE}"));
        match arg.as_str() {
            "--opend" => o.opend = value()?,
            "--key" => o.key = Some(value()?),
            "--resolution" => o.resolutions = value()?.split(',').map(|r| r.trim().to_ascii_uppercase()).filter(|r| !r.is_empty()).collect(),
            "--from" => {
                let v = value()?;
                o.from = Some(day_of(&v).ok_or_else(|| format!("{v} is not a day (dd/mm/yyyy or yyyy-mm-dd)"))?);
            }
            "--clickhouse" => o.clickhouse = Some(value()?),
            "--database" => o.database = value()?,
            "--user" => o.user = value()?,
            "--password" => o.password = value()?,
            "--file" => {
                let path = value()?;
                let text = std::fs::read_to_string(&path).map_err(|e| format!("{path} could not be read: {e}"))?;
                o.symbols.extend(symbols_in(&text));
            }
            "-h" | "--help" => return Err(USAGE.to_string()),
            s if s.starts_with("--") => return Err(format!("Unknown option {s}\n\n{USAGE}")),
            s => o.symbols.extend(symbols_in(s)),
        }
    }
    let wrong: Vec<&String> = o.symbols.iter().filter(|s| security_of(s).is_none()).collect();
    if !wrong.is_empty() {
        let list: Vec<&str> = wrong.iter().map(|s| s.as_str()).collect();
        return Err(format!("Not symbols of moomoo: {} (write them as US:AAPL, HK:00700, SH:600519, SZ:000001)", list.join(", ")));
    }
    if o.symbols.is_empty() {
        return Err(format!("No symbols given\n\n{USAGE}"));
    }
    if let Some(r) = o.resolutions.iter().find(|r| kl_type(r).is_none()) {
        return Err(format!("{r} is not a resolution OpenD serves ({})", bars::INTERVALS.join(", ")));
    }
    o.symbols.dedup();
    Ok(o)
}

/// How one symbol's backfill at one resolution went.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub symbol: String,
    pub resolution: String,
    /// The bars stored from the oldest one on, and the oldest one's time; or why OpenD refused.
    pub result: Result<(usize, Option<i64>), String>,
    /// Requests that asked OpenD.
    pub asked: usize,
}

impl std::fmt::Display for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.result {
            Ok((n, Some(t))) => write!(f, "{} {}: {n} bars stored, from {} ({} asked)", self.symbol, self.resolution, shown(*t), self.asked),
            Ok((_, None)) => write!(f, "{} {}: OpenD has no bars", self.symbol, self.resolution),
            Err(e) => write!(f, "{} {}: failed: {e}", self.symbol, self.resolution),
        }
    }
}

/// Fills the store with `symbols`' bars at `resolutions`, back to `from` or as far as `history`'s source has them.
pub async fn backfill<S: Source>(history: &History<S>, symbols: &[String], resolutions: &[String], from: Option<i64>, now: i64) -> Vec<Outcome> {
    let oldest = from.unwrap_or_else(|| day_of(OLDEST).unwrap_or(0));
    let mut out = Vec::new();
    for symbol in symbols {
        let Some((market, sec)) = security_of(symbol) else { continue };
        for resolution in resolutions {
            let Some((_, secs)) = kl_type(resolution) else { continue };
            let ask = |a: i64, b: i64| Ask { market, sec: &sec, symbol, resolution, from: a as f64, to: b as f64, count: 0 };
            let mut asked = 0;
            let result = if secs >= 86_400 {
                history.fill(&ask(oldest, now), now).await.map(|a| asked += a as usize)
            } else {
                // Newest step first, until one holds no bars (or `from`).
                let mut end = now;
                loop {
                    let start = (end - STEP).max(oldest);
                    match history.fill(&ask(start, end), now).await {
                        Ok(a) => asked += a as usize,
                        Err(e) => break Err(e),
                    }
                    if start <= oldest || (from.is_none() && history.stored(market, symbol, resolution, start, end).await.is_empty()) {
                        break Ok(());
                    }
                    end = start - 1;
                }
            };
            let stored = history.stored(market, symbol, resolution, oldest, now).await;
            let result = result.map(|()| (stored.len(), stored.first().map(|b| b[0] as i64)));
            let o = Outcome { symbol: symbol.clone(), resolution: resolution.clone(), result, asked };
            println!("{o}");
            out.push(o);
        }
    }
    out
}

fn backend_of(o: &Options) -> Result<Backend, String> {
    match &o.clickhouse {
        Some(url) => Ok(Backend::ClickHouse(ClickHouse::new(url, &o.database, &o.user, &o.password)?)),
        None => Ok(Backend::embedded()),
    }
}

/// Connects to OpenD and waits for InitConnect.
async fn connect(o: &Options) -> Result<(Client, tokio::sync::mpsc::UnboundedReceiver<Event>), String> {
    let (client, events) = Client::new(o.opend.clone());
    client.set_key(o.key.clone());
    tokio::spawn(client.clone().run());
    let deadline = tokio::time::Instant::now() + CONNECT_WAIT;
    while client.login().is_none() {
        if tokio::time::Instant::now() >= deadline {
            return Err(format!("OpenD is not reachable at {} (is it running and logged in?)", o.opend));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    Ok((client, events))
}

async fn print_quota(client: &Client, when: &str) {
    if let Ok(s2c) = client.request(proto::QOT_HISTORY_KL_QUOTA, json!({ "bGetDetail": false })).await {
        let n = |k: &str| s2c.get(k).and_then(Value::as_i64).unwrap_or(0);
        println!("History quota {when}: {} stocks used, {} left (rolling 7 days)", n("usedQuota"), n("remainQuota"));
    }
}

/// Runs a command; Ok(false) when some of it failed.
pub async fn run(o: Options) -> Result<bool, String> {
    let store = Arc::new(Store::new(backend_of(&o)?));
    let (client, mut events) = connect(&o).await?;
    match o.command {
        Command::Backfill => {
            // Nothing is subscribed: the events are only connections.
            tokio::spawn(async move { while events.recv().await.is_some() {} });
            print_quota(&client, "before").await;
            let history = History { source: bars::OpenD { client: client.clone(), pace: Pace::history() }, store: store.clone() };
            let outcomes = backfill(&history, &o.symbols, &o.resolutions, o.from, Utc::now().timestamp()).await;
            store.flush().await;
            print_quota(&client, "after").await;
            let failed = outcomes.iter().filter(|x| x.result.is_err()).count();
            if failed > 0 {
                println!("{failed} of {} failed", outcomes.len());
            }
            Ok(failed == 0)
        }
        Command::Record => record(&client, &mut events, &store, &o.symbols).await,
    }
}

/// Subscribes `symbols`' 1-minute K-lines and stores each closed bar until Ctrl+C.
pub async fn record(client: &Client, events: &mut tokio::sync::mpsc::UnboundedReceiver<Event>, store: &Store, symbols: &[String]) -> Result<bool, String> {
    let subscribe = || async {
        match client.request(proto::QOT_SUB, stream::sub_request_of(symbols, &[proto::SUB_KL_1MIN], true)).await {
            Ok(_) => println!("Recording 1-minute bars of {}", symbols.join(", ")),
            Err(e) => println!("[warn] OpenD refused the subscription: {e}"),
        }
    };
    subscribe().await;
    let cover = Mutex::new(MinuteCover::default());
    let mut quotes = Quotes::default();
    let mut flush = tokio::time::interval(FLUSH_EVERY);
    let mut since_flush = 0usize;
    loop {
        tokio::select! {
            e = events.recv() => match e {
                None => return Err("The connection to OpenD stopped".into()),
                Some(Event::Connected) => {
                    cover.lock().unwrap_or_else(|e| e.into_inner()).reset();
                    subscribe().await;
                }
                Some(Event::Push { proto: proto::QOT_UPDATE_KL, s2c }) => {
                    if let Some((symbol, bar)) = quotes.minute_push(&s2c, Utc::now().timestamp()) {
                        stream::store_minute(store, &cover, &symbol, bar).await;
                        since_flush += 1;
                    }
                }
                Some(_) => {}
            },
            _ = flush.tick() => {
                store.flush().await;
                if since_flush > 0 {
                    println!("{} {since_flush} closed minutes stored", Utc::now().format("%d/%m/%Y %H:%M:%S"));
                    since_flush = 0;
                }
            }
            _ = tokio::signal::ctrl_c() => {
                store.flush().await;
                println!("Stopped; {since_flush} closed minutes stored at the end");
                return Ok(true);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bars::unix_of;
    use crate::market::Market;
    use crate::opend::proto::{KLine, Security};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    /// One 1-minute bar at 10:00 New York each weekday of the last `days`, and daily bars likewise; `BAD` fails.
    /// `oldest_begin` is the oldest 1-minute ask.
    struct Fake {
        now: i64,
        days: i64,
        asks: AtomicUsize,
        oldest_begin: Mutex<i64>,
    }

    impl Source for Fake {
        async fn klines(&self, sec: &Security, kl: i32, _: bool, begin: String, end: String) -> Result<Vec<KLine>, String> {
            if sec.code == "BAD" {
                return Err("no quota left".into());
            }
            self.asks.fetch_add(1, Ordering::Relaxed);
            let (a, b) = (unix_of(Market::Us, &begin).unwrap(), unix_of(Market::Us, &end).unwrap());
            let daily = kl == 2;
            if !daily {
                let mut oldest = self.oldest_begin.lock().unwrap();
                *oldest = (*oldest).min(a);
            }
            let lines = (0..self.days)
                .filter_map(|d| {
                    let day = bars::wall_of(Market::Us, self.now - d * 86_400);
                    let date = NaiveDate::parse_from_str(&day[..10], "%Y-%m-%d").ok()?;
                    use chrono::Datelike;
                    if date.weekday().num_days_from_monday() >= 5 {
                        return None;
                    }
                    let time = format!("{} {}", &day[..10], if daily { "00:00:00" } else { "10:00:00" });
                    let t = unix_of(Market::Us, &time)?;
                    (t >= a && t <= b && t < self.now).then(|| {
                        serde_json::from_value(json!({"time":time,"isBlank":false,"openPrice":1,"highPrice":1,"lowPrice":1,"closePrice":1,"volume":"1"})).unwrap()
                    })
                })
                .collect();
            Ok(lines)
        }
    }

    fn history(days: i64, now: i64) -> History<Fake> {
        let fake = Fake { now, days, asks: AtomicUsize::new(0), oldest_begin: Mutex::new(i64::MAX) };
        History { source: fake, store: Arc::new(Store::new(Backend::Memory)) }
    }

    #[tokio::test]
    async fn a_backfill_goes_back_to_the_first_step_without_bars_and_a_second_asks_nothing() {
        let now = unix_of(Market::Us, "2025-05-09 21:00:00").unwrap();
        let h = history(70, now);
        let out = backfill(&h, &s(&["US:AAPL"]), &s(&["1", "1D"]), None, now).await;
        let (minutes, oldest) = out[0].result.clone().unwrap();
        // 70 days hold 50 weekdays.
        assert_eq!((minutes, out[1].result.clone().unwrap().0), (50, 50));
        assert!(oldest.unwrap() >= now - 70 * 86_400);
        // Steps of 30 days: three hold bars, the fourth none; nothing older is asked.
        assert_eq!(out[0].asked, 4);
        assert!(*h.source.oldest_begin.lock().unwrap() >= now - 4 * STEP - 120, "minutes asked no further than the empty step");
        let asks = h.source.asks.load(Ordering::Relaxed);
        let again = backfill(&h, &s(&["US:AAPL"]), &s(&["1", "1D"]), None, now).await;
        // Only today's daily bar, still forming at `now`, is asked again.
        assert_eq!((again[0].asked, again[1].asked), (0, 1));
        assert_eq!(h.source.asks.load(Ordering::Relaxed), asks + 1);
    }

    #[tokio::test]
    async fn a_refused_symbol_fails_and_the_next_is_filled() {
        let now = unix_of(Market::Us, "2025-05-09 21:00:00").unwrap();
        let h = history(10, now);
        let out = backfill(&h, &s(&["US:BAD", "US:AAPL"]), &s(&["1D"]), None, now).await;
        assert_eq!(out[0].result, Err("no quota left".into()));
        assert_eq!(out[1].result.clone().unwrap().0, 8);
        assert!(out[0].to_string().contains("failed"));
    }

    #[test]
    fn the_command_line_reads_symbols_options_and_refuses_strangers() {
        let args = s(&["backfill", "us:aapl,HK:00700", "--from", "01/02/2024", "--resolution", "1,1d", "--clickhouse", "http://localhost:8123"]);
        let o = parse(&args).unwrap();
        assert_eq!((o.command, o.symbols, o.resolutions), (Command::Backfill, s(&["US:AAPL", "HK:00700"]), s(&["1", "1D"])));
        assert_eq!(o.from, day_of("2024-02-01"));
        assert_eq!((o.clickhouse.as_deref(), o.database.as_str()), (Some("http://localhost:8123"), "trading_db"));
        let e = parse(&s(&["record", "AAPL", "XX:NOPE"])).unwrap_err();
        assert!(e.contains("AAPL, XX:NOPE") && e.contains("US:AAPL"), "{e}");
        assert!(parse(&s(&["record"])).unwrap_err().contains("No symbols"));
        assert!(parse(&s(&["backfill", "US:AAPL", "--resolution", "2"])).is_err());
        assert!(parse(&s(&["nope"])).unwrap_err().starts_with("Usage"));
        assert_eq!(symbols_in("US:AAPL # apple\n\nHK:00700, SH:600519"), s(&["US:AAPL", "HK:00700", "SH:600519"]));
    }
}
