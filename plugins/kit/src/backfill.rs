/*
 * `opentrader-<plugin> backfill`: the plugin program as a command, outside the app, filling the same bar store the
 * charts read (the plugin's SQLite file, or ClickHouse with --clickhouse). It logs in on its own from the
 * credentials in the environment (or an --env file), so it runs unattended (cron, launchd). Run again later, it asks
 * only for the bars the store is missing.
 *
 *   opentrader-zerodha backfill NSE:RELIANCE NSE:NIFTY_50 [--file list.txt] [--from 01/01/2020]
 *                               [--resolutions 1,1D] [--clickhouse http://localhost:8123 --database trading_db
 *                               --user default --password …] [--env ~/.config/opentrader/zerodha.env]
 *
 * Exit status: 0 when every symbol was filled, 1 when one failed, 2 for wrong arguments.
 */
use crate::catalog::Catalog;
use crate::feed::Provider;
use crate::history::{History, Span};
use crate::store::{backend_of, Backend, Key, Store};
use chrono::{NaiveDate, TimeZone};
use std::collections::HashMap;
use std::sync::Arc;

pub const USAGE: &str = "backfill SYMBOL… [--file FILE] [--from dd/mm/yyyy] [--resolutions 1,1D] \
[--clickhouse URL] [--database DB] [--user USER] [--password PASSWORD] [--env FILE]";

/// Bars before this are not asked by default: the broker's history ends earlier, which a step with none shows.
const DEFAULT_FROM: &str = "01/01/2000";

#[derive(Debug, Clone, PartialEq)]
pub struct Opts {
    pub symbols: Vec<String>,
    /// --from: bars from its midnight in each exchange's zone.
    pub from: NaiveDate,
    pub resolutions: Vec<String>,
    /// The store settings (`clickhouseUrl`…), as in Settings > Plugins.
    pub store: HashMap<String, String>,
    pub env_file: Option<String>,
}

/// A dd/mm/yyyy date.
pub fn date_of(text: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(text.trim(), "%d/%m/%Y").ok()
}

/// The command line after `backfill`. Symbols come from the arguments and --file (one or more a line, comma or
/// space separated, `#` comments).
pub fn parse(args: &[String]) -> Result<Opts, String> {
    let mut opts = Opts { symbols: Vec::new(), from: NaiveDate::MIN, resolutions: vec!["1".into(), "1D".into()], store: HashMap::new(), env_file: None };
    let mut from = DEFAULT_FROM.to_string();
    let mut list = String::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = |name: &str| it.next().cloned().ok_or_else(|| format!("{name} needs a value"));
        match a.as_str() {
            "--file" => {
                let f = value("--file")?;
                let text = std::fs::read_to_string(&f).map_err(|e| format!("{f}: {e}"))?;
                list.push_str(&text);
                list.push('\n');
            }
            "--from" => from = value("--from")?,
            "--resolutions" => opts.resolutions = value("--resolutions")?.split(',').map(|r| r.trim().to_string()).filter(|r| !r.is_empty()).collect(),
            "--clickhouse" => _ = opts.store.insert("clickhouseUrl".into(), value("--clickhouse")?),
            "--database" => _ = opts.store.insert("clickhouseDatabase".into(), value("--database")?),
            "--user" => _ = opts.store.insert("clickhouseUser".into(), value("--user")?),
            "--password" => _ = opts.store.insert("clickhousePassword".into(), value("--password")?),
            "--env" => opts.env_file = Some(value("--env")?),
            flag if flag.starts_with("--") => return Err(format!("Unknown option {flag}")),
            symbol => {
                list.push_str(symbol);
                list.push('\n');
            }
        }
    }
    let text: String = list.lines().map(|l| l.split('#').next().unwrap_or("")).collect::<Vec<_>>().join(",");
    for e in text.split(|c: char| c == ',' || c == ';' || c.is_whitespace()).filter(|e| !e.is_empty()) {
        match e.split_once(':') {
            Some((x, t)) if !x.is_empty() && !t.is_empty() => {
                let s = e.to_ascii_uppercase();
                if !opts.symbols.contains(&s) {
                    opts.symbols.push(s);
                }
            }
            _ => return Err(format!("{e}: not a symbol (EXCHANGE:TICKER, such as NSE:RELIANCE)")),
        }
    }
    if opts.symbols.is_empty() {
        return Err("No symbols: name them (NSE:RELIANCE) or give --file".into());
    }
    opts.from = date_of(&from).ok_or_else(|| format!("--from {from}: not a dd/mm/yyyy date"))?;
    if opts.resolutions.is_empty() {
        return Err("--resolutions names none".into());
    }
    Ok(opts)
}

/// KEY=VALUE lines of `path` into the environment, for those not already set (blank lines and `#` comments skipped;
/// `export ` and quotes allowed). Called before any thread starts.
pub fn load_env_file(path: &str) -> Result<usize, String> {
    let path = match path.strip_prefix("~/") {
        Some(rest) => dirs::home_dir().ok_or("no home folder")?.join(rest),
        None => path.into(),
    };
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut set = 0;
    for line in text.lines() {
        let line = line.trim();
        let line = line.strip_prefix("export ").unwrap_or(line);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let (k, v) = (k.trim(), v.trim().trim_matches('"').trim_matches('\''));
        if !k.is_empty() && std::env::var_os(k).is_none() {
            std::env::set_var(k, v);
            set += 1;
        }
    }
    Ok(set)
}

/// Fills the store for `opts`; the number of symbols that failed.
pub async fn run<P: Provider>(provider: P, opts: &Opts) -> Result<usize, String> {
    crate::set_plugin(P::ID);
    // No settings: the credentials come from the environment.
    provider.configure(&HashMap::new()).await;
    let provider = Arc::new(provider);
    let backend = backend_of(&opts.store);
    if !opts.store.is_empty() && backend.is_none() {
        return Err("the ClickHouse store could not be set up".into());
    }
    let store = Arc::new(Store::new(backend.unwrap_or_else(Backend::embedded)));
    let catalog = Catalog::open(crate::store::cache_dir().map(|d| d.join("instruments.json")));
    let now = chrono::Utc::now().timestamp();
    let p = provider.clone();
    if let Err(e) = catalog.keep_fresh(now, || async move { p.instruments().await }).await {
        if catalog.is_empty() {
            return Err(format!("the instrument list: {e}"));
        }
        println!("[warn] the instrument list was not loaded again ({e}); using the one on disk");
    }
    let history = History { source: provider.clone(), store: store.clone() };
    let mut failed = 0;
    for symbol in &opts.symbols {
        let Some(inst) = catalog.find(symbol) else {
            println!("[error] {symbol}: not in the instrument list");
            failed += 1;
            continue;
        };
        let cal = P::calendar(inst.exchange());
        let from = cal.midnight_of(opts.from);
        for res in &opts.resolutions {
            let started = std::time::Instant::now();
            let refused = history.fill(&inst, &cal, res, Span { from, to: now, count: 0 }, now).await;
            let (stored, first) = store.read(&Key::new(&inst.symbol, res, false), |s| s.count(from, now)).await;
            match refused {
                Ok(refused) if stored > 0 || refused.is_none() => {
                    if let Some(e) = refused {
                        println!("[warn] {} {res}: {e}; the stored bars stay", inst.symbol);
                    }
                    let first = first.and_then(|t| cal.tz.timestamp_opt(t, 0).single()).map(|d| format!(" from {}", d.format("%d/%m/%Y")));
                    println!("[info] {} {res}: {stored} bars stored{} ({:.1}s)", inst.symbol, first.unwrap_or_default(), started.elapsed().as_secs_f64());
                }
                Ok(refused) => {
                    println!("[error] {} {res}: {}", inst.symbol, refused.unwrap_or_default());
                    failed += 1;
                }
                Err(e) => {
                    println!("[error] {} {res}: {e}", inst.symbol);
                    failed += 1;
                }
            }
            // Written per series: a stopped run keeps what it has.
            store.flush().await;
        }
    }
    Ok(failed)
}

/// `backfill` of the plugin program: the exit status.
pub fn main<P: Provider>(provider: P, args: &[String]) -> i32 {
    let opts = match parse(args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("{e}\nUsage: opentrader-{} {USAGE}", P::ID);
            return 2;
        }
    };
    if let Some(f) = &opts.env_file {
        if let Err(e) = load_env_file(f) {
            eprintln!("--env {e}");
            return 2;
        }
    }
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Cannot start: {e}");
            return 1;
        }
    };
    match runtime.block_on(run(provider, &opts)) {
        Ok(0) => 0,
        Ok(n) => {
            println!("[error] {n} of the series failed");
            1
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_command_line_is_read() {
        let o = parse(&args(&["nse:reliance", "NSE:INFY,NSE:TCS", "--from", "01/01/2025", "--resolutions", "1D", "--clickhouse", "http://localhost:8123"]))
            .unwrap();
        assert_eq!(o.symbols, ["NSE:RELIANCE", "NSE:INFY", "NSE:TCS"]);
        assert_eq!(o.from, NaiveDate::from_ymd_opt(2025, 1, 1).unwrap());
        assert_eq!(o.resolutions, ["1D"]);
        assert_eq!(o.store.get("clickhouseUrl").map(String::as_str), Some("http://localhost:8123"));
        let d = parse(&args(&["NSE:INFY"])).unwrap();
        assert_eq!((d.resolutions.clone(), d.from), (vec!["1".to_string(), "1D".to_string()], date_of(DEFAULT_FROM).unwrap()));
    }

    #[test]
    fn wrong_command_lines_are_named() {
        assert!(parse(&[]).unwrap_err().starts_with("No symbols"));
        assert_eq!(parse(&args(&["NSE:INFY", "--from", "2025-01-01"])).unwrap_err(), "--from 2025-01-01: not a dd/mm/yyyy date");
        assert_eq!(parse(&args(&["NSE:INFY", "--fast"])).unwrap_err(), "Unknown option --fast");
        assert_eq!(parse(&args(&["NSE:INFY", "--from"])).unwrap_err(), "--from needs a value");
    }

    #[test]
    fn symbol_files_and_env_files_are_read() {
        let dir = std::env::temp_dir().join(format!("kit-backfill-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let list = dir.join("list.txt");
        std::fs::write(&list, "# watchlist\nNSE:RELIANCE NSE:INFY\nNFO:NIFTY26NOVFUT # the future\n").unwrap();
        let o = parse(&args(&["--file", list.to_str().unwrap()])).unwrap();
        assert_eq!(o.symbols, ["NSE:RELIANCE", "NSE:INFY", "NFO:NIFTY26NOVFUT"]);

        let env = dir.join("x.env");
        std::fs::write(&env, "# creds\nexport KIT_TEST_A=\"one\"\nKIT_TEST_B=two\n\nnot a line\n").unwrap();
        assert_eq!(load_env_file(env.to_str().unwrap()).unwrap(), 2);
        assert_eq!(std::env::var("KIT_TEST_A").unwrap(), "one");
        assert_eq!(load_env_file(env.to_str().unwrap()).unwrap(), 0, "set ones are kept");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
