/*
 * A broker's instrument list (static data): loaded from the broker, kept on disk in the plugin's folder and read
 * back at start, loaded again when a day old. It names symbols (`data.symbolInfo`), finds the instrument of a
 * symbol and of a stream key (a tick's token), and answers `data.search`: exact trading symbol, then prefix, then
 * every query word found in the symbol or the name (`NIFTY 24000 CE`); cash and indices before derivatives, nearer
 * expiries first.
 */
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

pub const MAX_ROWS: usize = 50;
/// How long a list is used before it is loaded again.
pub const FRESH_SECS: i64 = 20 * 3600;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Instrument {
    /// The app's symbol, `NSE:RELIANCE`.
    pub symbol: String,
    /// The description shown in search and on the chart.
    pub name: String,
    /// `stock`, `index`, `fund` or `futures` (futures and options).
    pub kind: String,
    /// The broker's id of it (a Kite instrument token, a Dhan security id).
    pub id: String,
    /// The broker's segment (Kite `NFO-OPT`, Dhan `NSE_FNO`).
    pub segment: String,
    /// The broker's instrument type (Kite `CE`, Dhan `OPTIDX`).
    pub instrument: String,
    /// The key a stream's tick names it by (unique in the list).
    pub key: String,
    pub tick: f64,
    pub lot: f64,
    /// `YYYY-MM-DD`.
    pub expiry: Option<String>,
    pub strike: Option<f64>,
    /// `CE` or `PE`.
    pub option: Option<String>,
}

impl Instrument {
    /// The exchange part of the symbol.
    pub fn exchange(&self) -> &str {
        self.symbol.split_once(':').map_or("", |(e, _)| e)
    }

    fn ticker(&self) -> &str {
        self.symbol.split_once(':').map_or(self.symbol.as_str(), |(_, t)| t)
    }

    /// One tick as `data.symbolInfo`'s pricescale: 0.05 → 100.
    pub fn pricescale(&self) -> u64 {
        let tick = if self.tick > 0.0 { self.tick } else { 0.05 };
        let mut scale = 1u64;
        while scale < 100_000_000 && ((tick * scale as f64) - (tick * scale as f64).round()).abs() > 1e-6 {
            scale *= 10;
        }
        scale
    }
}

/// A trading symbol as the app's ticker: upper case, spaces as `_` (`NIFTY 50` → `NIFTY_50`).
pub fn ticker_of(trading_symbol: &str) -> String {
    trading_symbol.trim().to_ascii_uppercase().split_whitespace().collect::<Vec<_>>().join("_")
}

#[derive(Serialize, Deserialize)]
struct File {
    fetched: i64,
    list: Vec<Instrument>,
}

#[derive(Default)]
struct Index {
    list: Vec<Instrument>,
    by_symbol: HashMap<String, usize>,
    by_key: HashMap<String, usize>,
    /// Per instrument: what the query words are looked for in.
    text: Vec<String>,
}

impl Index {
    fn new(mut list: Vec<Instrument>) -> Index {
        // One instrument per symbol: a repeated one could not be named.
        let mut seen = std::collections::HashSet::with_capacity(list.len());
        let before = list.len();
        list.retain(|i| seen.insert(i.symbol.clone()));
        if list.len() < before {
            println!("[warn] {} instruments repeat a symbol and are left out", before - list.len());
        }
        let mut by_symbol = HashMap::with_capacity(list.len());
        let mut by_key = HashMap::with_capacity(list.len());
        let mut text = Vec::with_capacity(list.len());
        for (i, inst) in list.iter().enumerate() {
            by_symbol.entry(inst.symbol.clone()).or_insert(i);
            by_key.entry(inst.key.clone()).or_insert(i);
            let strike = inst.strike.filter(|s| *s > 0.0).map(|s| format!("{s}")).unwrap_or_default();
            text.push(
                format!("{} {} {} {} {}", inst.ticker(), inst.name, strike, inst.option.as_deref().unwrap_or(""), inst.expiry.as_deref().unwrap_or(""))
                    .to_ascii_uppercase(),
            );
        }
        Index { list, by_symbol, by_key, text }
    }
}

pub struct Catalog {
    index: RwLock<Arc<Index>>,
    file: Option<PathBuf>,
    fetched: RwLock<Option<i64>>,
    loading: tokio::sync::Mutex<()>,
}

impl Catalog {
    /// The list of `file` when it holds one; empty otherwise.
    pub fn open(file: Option<PathBuf>) -> Catalog {
        let read = file.as_ref().and_then(|f| std::fs::read(f).ok()).and_then(|b| serde_json::from_slice::<File>(&b).ok());
        let (list, fetched) = match read {
            Some(f) if !f.list.is_empty() => (f.list, Some(f.fetched)),
            _ => (Vec::new(), None),
        };
        Catalog { index: RwLock::new(Arc::new(Index::new(list))), file, fetched: RwLock::new(fetched), loading: tokio::sync::Mutex::new(()) }
    }

    fn index(&self) -> Arc<Index> {
        self.index.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn len(&self) -> usize {
        self.index().list.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Loads the list with `load` when there is none or it is older than `FRESH_SECS`, and writes it to the file.
    /// Ok(true) when it was loaded. One load at a time; a failed load keeps the list in use.
    pub async fn keep_fresh<F, Fut>(&self, now: i64, load: F) -> Result<bool, String>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<Vec<Instrument>, String>>,
    {
        let _one = self.loading.lock().await;
        let fetched = *self.fetched.read().unwrap_or_else(|e| e.into_inner());
        if fetched.is_some_and(|t| now - t < FRESH_SECS) && !self.is_empty() {
            return Ok(false);
        }
        let list = load().await?;
        if list.is_empty() {
            return Err("the broker listed no instruments".into());
        }
        if let Some(file) = &self.file {
            let body = serde_json::to_vec(&File { fetched: now, list: list.clone() }).unwrap_or_default();
            let written = file.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|_| {
                let tmp = file.with_extension("tmp");
                std::fs::write(&tmp, body)?;
                std::fs::rename(&tmp, file)
            });
            if let Err(e) = written {
                println!("[warn] the instrument list could not be written to {}: {e}", file.display());
            }
        }
        println!("[info] loaded {} instruments", list.len());
        let index = Index::new(list);
        *self.index.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(index);
        *self.fetched.write().unwrap_or_else(|e| e.into_inner()) = Some(now);
        Ok(true)
    }

    pub fn find(&self, symbol: &str) -> Option<Instrument> {
        let index = self.index();
        let symbol = symbol.trim().to_ascii_uppercase();
        index.by_symbol.get(&symbol).map(|&i| index.list[i].clone())
    }

    pub fn by_key(&self, key: &str) -> Option<Instrument> {
        let index = self.index();
        index.by_key.get(key).map(|&i| index.list[i].clone())
    }

    /// The best `MAX_ROWS` for `query`, as `data.search` rows.
    pub fn search(&self, query: &str) -> Vec<Value> {
        let index = self.index();
        let q = query.trim().to_ascii_uppercase();
        let words: Vec<&str> = q.split_whitespace().collect();
        if words.is_empty() {
            return Vec::new();
        }
        let joined = words.join("_");
        let mut hits: Vec<(u8, u8, &str, usize, usize)> = Vec::new();
        for (i, inst) in index.list.iter().enumerate() {
            let ticker = inst.ticker();
            let score = if ticker == joined || ticker == words.concat() {
                0
            } else if ticker.starts_with(&joined) {
                1
            } else if words.iter().all(|w| ticker.contains(w)) {
                2
            } else if words.iter().all(|w| index.text[i].contains(w)) {
                3
            } else {
                continue;
            };
            // Cash and indices first, then the main exchange (NSE before BSE).
            let class = u8::from(inst.kind == "futures") * 2 + u8::from(!matches!(inst.exchange(), "NSE" | "NFO"));
            hits.push((score, class, inst.expiry.as_deref().unwrap_or(""), ticker.len(), i));
        }
        hits.sort_by(|a, b| (a.0, a.1, a.2, a.3, &index.list[a.4].symbol).cmp(&(b.0, b.1, b.2, b.3, &index.list[b.4].symbol)));
        hits.into_iter()
            .take(MAX_ROWS)
            .map(|h| {
                let inst = &index.list[h.4];
                json!({ "ticker": inst.symbol, "description": inst.name, "type": inst.kind })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn inst(symbol: &str, name: &str, kind: &str, expiry: Option<&str>, strike: Option<f64>, option: Option<&str>) -> Instrument {
        Instrument {
            symbol: symbol.into(),
            name: name.into(),
            kind: kind.into(),
            key: symbol.into(),
            expiry: expiry.map(Into::into),
            strike,
            option: option.map(Into::into),
            tick: 0.05,
            ..Default::default()
        }
    }

    fn catalog() -> Catalog {
        let c = Catalog::open(None);
        let list = vec![
            inst("NFO:RELIANCE26NOVFUT", "RELIANCE", "futures", Some("2026-11-26"), None, None),
            inst("BSE:RELIANCE", "RELIANCE INDUSTRIES", "stock", None, None, None),
            inst("NSE:RELIANCE", "RELIANCE INDUSTRIES", "stock", None, None, None),
            inst("NSE:RELINFRA", "RELIANCE INFRASTRUCTURE", "stock", None, None, None),
            inst("NFO:NIFTY26DEC24000CE", "NIFTY", "futures", Some("2026-12-31"), Some(24000.0), Some("CE")),
            inst("NFO:NIFTY26N2424000CE", "NIFTY", "futures", Some("2026-11-24"), Some(24000.0), Some("CE")),
            inst("NFO:NIFTY26N2424000PE", "NIFTY", "futures", Some("2026-11-24"), Some(24000.0), Some("PE")),
            inst("NFO:NIFTY26N2424100CE", "NIFTY", "futures", Some("2026-11-24"), Some(24100.0), Some("CE")),
            inst("NSE:NIFTY_50", "NIFTY 50", "index", None, None, None),
        ];
        futures_block(c.keep_fresh(0, || async { Ok(list) })).unwrap();
        c
    }

    fn futures_block<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(f)
    }

    fn tickers(rows: &[Value]) -> Vec<&str> {
        rows.iter().map(|r| r["ticker"].as_str().unwrap()).collect()
    }

    #[test]
    fn exact_symbol_first_then_cash_before_derivatives() {
        let c = catalog();
        assert_eq!(tickers(&c.search("reliance"))[..4], ["NSE:RELIANCE", "BSE:RELIANCE", "NFO:RELIANCE26NOVFUT", "NSE:RELINFRA"]);
    }

    #[test]
    fn words_find_a_strike_nearer_expiry_first() {
        let c = catalog();
        assert_eq!(tickers(&c.search("nifty 24000 ce")), ["NFO:NIFTY26N2424000CE", "NFO:NIFTY26DEC24000CE"]);
        assert_eq!(tickers(&c.search("nifty 50")), ["NSE:NIFTY_50"]);
    }

    #[test]
    fn found_by_symbol_and_key_and_kept_on_disk() {
        let dir = std::env::temp_dir().join(format!("ot-kit-catalog-{}", std::process::id()));
        let file = dir.join("instruments.json");
        let _ = std::fs::remove_dir_all(&dir);
        let c = Catalog::open(Some(file.clone()));
        let loaded = futures_block(c.keep_fresh(100, || async { Ok(vec![inst("NSE:INFY", "INFOSYS", "stock", None, None, None)]) }));
        assert_eq!(loaded, Ok(true));
        assert_eq!(c.find("nse:infy").map(|i| i.name), Some("INFOSYS".into()));
        assert_eq!(c.by_key("NSE:INFY").map(|i| i.kind), Some("stock".into()));
        // A later run reads the file; fresh, it does not load.
        let again = Catalog::open(Some(file));
        assert_eq!(again.len(), 1);
        assert_eq!(futures_block(again.keep_fresh(200, || async { Err("asked".to_string()) })), Ok(false));
        assert!(futures_block(again.keep_fresh(100 + FRESH_SECS, || async { Err("asked".to_string()) })).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tickers_and_price_scales() {
        assert_eq!(ticker_of(" Nifty 50 "), "NIFTY_50");
        assert_eq!(inst("A:B", "", "stock", None, None, None).pricescale(), 100);
        let mut i = inst("A:B", "", "stock", None, None, None);
        i.tick = 0.0025;
        assert_eq!(i.pricescale(), 10_000);
        i.tick = 1.0;
        assert_eq!(i.pricescale(), 1);
    }
}
