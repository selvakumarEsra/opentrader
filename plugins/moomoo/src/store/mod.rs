/*
 * The local OHLCV store. Per series — (symbol, resolution, session) — the bars known and the ranges of bar start
 * times the store is sure of ("covered": every bar starting in it is here and closed).
 *
 * A series is loaded whole from the backend on first use and kept in memory; its changes (bars added, ranges, a
 * drop) are written back by `flush`, so a minute of streamed bars costs one write. The backend is ClickHouse when
 * the user gives its URL, else an embedded SQLite file; both read a series in one range scan of their primary key.
 */
pub mod clickhouse;
pub mod sqlite;

use crate::bars::Bar;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Series {
    /// Oldest first, one per start time.
    bars: Vec<Bar>,
    /// Sorted, disjoint, not adjacent; inclusive bounds.
    covered: Vec<(i64, i64)>,
    /// Bars merged since the last flush.
    added: Vec<Bar>,
    /// Dropped since the last flush: the backend deletes what it holds first.
    cleared: bool,
}

/// What a flush writes for one series.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Change {
    pub cleared: bool,
    pub added: Vec<Bar>,
    pub covered: Vec<(i64, i64)>,
}

impl Series {
    /// A series as a backend read it.
    pub fn loaded(mut bars: Vec<Bar>, covered: Vec<(i64, i64)>) -> Self {
        bars.sort_by(|a, b| a[0].total_cmp(&b[0]));
        bars.dedup_by(|later, first| later[0] == first[0]);
        let mut s = Series { bars, ..Default::default() };
        for (a, b) in covered {
            s.cover(a, b);
        }
        s
    }

    /// Adds or replaces bars by start time.
    pub fn merge(&mut self, bars: &[Bar]) {
        if bars.is_empty() {
            return;
        }
        let mut all: Vec<Bar> = bars.to_vec();
        // The new bars win over stored ones of the same time: dedup keeps the first.
        all.extend(self.bars.iter().copied());
        all.sort_by(|a, b| a[0].total_cmp(&b[0]));
        all.dedup_by(|later, first| later[0] == first[0]);
        self.bars = all;
        self.added.extend_from_slice(bars);
    }

    /// Forgets everything: the adjusted prices changed under it.
    pub fn clear(&mut self) {
        *self = Series { cleared: true, ..Default::default() };
    }

    pub fn cover(&mut self, a: i64, b: i64) {
        if b < a {
            return;
        }
        self.covered.push((a, b));
        self.covered.sort_unstable();
        let mut merged: Vec<(i64, i64)> = Vec::with_capacity(self.covered.len());
        for (a, b) in self.covered.drain(..) {
            match merged.last_mut() {
                Some(last) if a <= last.1.saturating_add(1) => last.1 = last.1.max(b),
                _ => merged.push((a, b)),
            }
        }
        self.covered = merged;
    }

    /// The parts of [a, b] not covered, oldest first.
    pub fn missing(&self, a: i64, b: i64) -> Vec<(i64, i64)> {
        let mut out = Vec::new();
        let mut from = a;
        for &(ca, cb) in &self.covered {
            if cb < from {
                continue;
            }
            if ca > b {
                break;
            }
            if ca > from {
                out.push((from, ca - 1));
            }
            from = from.max(cb.saturating_add(1));
            if from > b {
                return out;
            }
        }
        if from <= b {
            out.push((from, b));
        }
        out
    }

    /// The bars starting in [a, b].
    pub fn window(&self, a: i64, b: i64) -> Vec<Bar> {
        let lo = self.bars.partition_point(|x| (x[0] as i64) < a);
        let hi = self.bars.partition_point(|x| (x[0] as i64) <= b);
        self.bars[lo..hi.max(lo)].to_vec()
    }

    pub fn at(&self, t: i64) -> Option<&Bar> {
        let i = self.bars.binary_search_by(|x| x[0].total_cmp(&(t as f64))).ok()?;
        self.bars.get(i)
    }

    pub fn covered_at(&self, t: i64) -> bool {
        self.covered.iter().any(|&(a, b)| a <= t && t <= b)
    }

    pub fn covered(&self) -> &[(i64, i64)] {
        &self.covered
    }

    fn take_change(&mut self) -> Change {
        Change { cleared: std::mem::take(&mut self.cleared), added: std::mem::take(&mut self.added), covered: self.covered.clone() }
    }
}

/// Which series: `US:AAPL`, `"5"`, extended hours or not.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Key {
    pub symbol: String,
    pub resolution: String,
    pub extended: bool,
}

impl Key {
    pub fn new(symbol: &str, resolution: &str, extended: bool) -> Self {
        Self { symbol: symbol.trim().to_ascii_uppercase(), resolution: resolution.trim().to_ascii_uppercase(), extended }
    }

    /// One string naming the series: `US:AAPL|5|ext`.
    pub fn id(&self) -> String {
        format!("{}|{}|{}", self.symbol, self.resolution, self.session())
    }

    pub fn session(&self) -> &'static str {
        if self.extended {
            "ext"
        } else {
            "rth"
        }
    }

    /// (exchange, ticker).
    pub fn parts(&self) -> (&str, &str) {
        self.symbol.split_once(':').unwrap_or(("", &self.symbol))
    }
}

/// Where series are kept.
pub enum Backend {
    /// Nothing written (tests; no cache folder).
    Memory,
    Sqlite(sqlite::Sqlite),
    ClickHouse(clickhouse::ClickHouse),
}

impl Backend {
    /// The embedded store in `dir` (`OPENTRADER_MOOMOO_CACHE`, else the user's cache folder), or memory when no
    /// folder can be had.
    pub fn embedded() -> Backend {
        let dir = match std::env::var_os("OPENTRADER_MOOMOO_CACHE") {
            Some(d) if !d.is_empty() => Some(PathBuf::from(d)),
            _ => dirs::cache_dir().map(|d| d.join("opentrader-moomoo")),
        };
        match dir.map(|d| sqlite::Sqlite::open(&d.join("bars.sqlite"))) {
            Some(Ok(s)) => Backend::Sqlite(s),
            Some(Err(e)) => {
                println!("[warn] the bar store could not be opened ({e}); bars are kept for this session only");
                Backend::Memory
            }
            None => Backend::Memory,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Backend::Memory => "memory",
            Backend::Sqlite(_) => "SQLite",
            Backend::ClickHouse(_) => "ClickHouse",
        }
    }

    async fn load(&self, key: &Key) -> Result<Series, String> {
        match self {
            Backend::Memory => Ok(Series::default()),
            Backend::Sqlite(s) => s.load(key).await,
            Backend::ClickHouse(c) => c.load(key).await,
        }
    }

    async fn save(&self, key: &Key, change: &Change) -> Result<(), String> {
        match self {
            Backend::Memory => Ok(()),
            Backend::Sqlite(s) => s.save(key, change).await,
            Backend::ClickHouse(c) => c.save(key, change).await,
        }
    }
}

pub struct Store {
    backend: tokio::sync::RwLock<Arc<Backend>>,
    series: Mutex<HashMap<Key, Series>>,
    dirty: Mutex<HashSet<Key>>,
}

fn locked<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Store {
    pub fn new(backend: Backend) -> Self {
        Self { backend: tokio::sync::RwLock::new(Arc::new(backend)), series: Mutex::new(HashMap::new()), dirty: Mutex::new(HashSet::new()) }
    }

    /// Moves to another backend: what is pending goes to the old one, and series are read from the new one.
    pub async fn set_backend(&self, backend: Backend) {
        self.flush().await;
        println!("[info] bars are stored in {}", backend.name());
        *self.backend.write().await = Arc::new(backend);
        locked(&self.series).clear();
    }

    async fn ensure(&self, key: &Key) {
        if locked(&self.series).contains_key(key) {
            return;
        }
        let backend = self.backend.read().await.clone();
        let loaded = backend.load(key).await.unwrap_or_else(|e| {
            println!("[warn] the stored bars of {} could not be read: {e}", key.symbol);
            Series::default()
        });
        locked(&self.series).entry(key.clone()).or_insert(loaded);
    }

    pub async fn read<R>(&self, key: &Key, f: impl FnOnce(&Series) -> R) -> R {
        self.ensure(key).await;
        let all = locked(&self.series);
        f(all.get(key).expect("ensured above"))
    }

    /// Changes `key`'s series; it is written at the next `flush`.
    pub async fn update<R>(&self, key: &Key, f: impl FnOnce(&mut Series) -> R) -> R {
        self.ensure(key).await;
        let r = f(locked(&self.series).get_mut(key).expect("ensured above"));
        locked(&self.dirty).insert(key.clone());
        r
    }

    /// Writes the changed series to the backend.
    pub async fn flush(&self) {
        let keys: Vec<Key> = locked(&self.dirty).drain().collect();
        if keys.is_empty() {
            return;
        }
        let backend = self.backend.read().await.clone();
        for key in keys {
            let change = locked(&self.series).get_mut(&key).map(Series::take_change);
            let Some(change) = change else { continue };
            if let Err(e) = backend.save(&key, &change).await {
                println!("[warn] the bars of {} could not be stored in {}: {e}", key.symbol, backend.name());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn bar(t: i64, c: f64) -> Bar {
        [t as f64, c, c, c, c, 1.0]
    }

    #[test]
    fn ranges_merge_and_gaps_are_missing() {
        let mut s = Series::default();
        s.cover(100, 199);
        s.cover(300, 399);
        s.cover(200, 249);
        assert_eq!(s.missing(0, 500), [(0, 99), (250, 299), (400, 500)]);
        assert_eq!(s.missing(120, 240), []);
        s.cover(250, 299);
        assert_eq!(s.missing(100, 399), []);
        assert_eq!(s.covered(), [(100, 399)]);
    }

    #[test]
    fn newer_bars_replace_stored_ones_and_are_the_change() {
        let mut s = Series::default();
        s.merge(&[bar(60, 1.0), bar(0, 0.5)]);
        s.merge(&[bar(60, 2.0), bar(120, 3.0)]);
        assert_eq!(s.window(0, 200).iter().map(|b| b[4]).collect::<Vec<_>>(), [0.5, 2.0, 3.0]);
        assert_eq!(s.window(60, 60).len(), 1);
        assert_eq!(s.at(120).map(|b| b[4]), Some(3.0));
        assert!(s.at(30).is_none());
        let c = s.take_change();
        assert_eq!(c.added.len(), 4);
        assert!(s.take_change().added.is_empty());
        s.clear();
        assert!(s.take_change().cleared);
    }

    #[test]
    fn keys_name_their_series() {
        let k = Key::new("us:aapl", "1d", false);
        assert_eq!((k.id(), k.parts()), ("US:AAPL|1D|rth".to_string(), ("US", "AAPL")));
    }
}
