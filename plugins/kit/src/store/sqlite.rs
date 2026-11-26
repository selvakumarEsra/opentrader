/*
 * The embedded store: one SQLite file, in a compact layout (specs/compact-bar-store.md):
 *   series   each series' name (`US:AAPL|1|ext`) once, with a number
 *   bars     clustered on (series number, t) (WITHOUT ROWID), so a series is one range scan of the primary key. A
 *            bar's prices are whole numbers of a decimal step `x` (10^-x) when that gives them back bit for bit, else
 *            decimals (`x` NULL); a whole volume is a whole number. SQLite keeps small whole numbers in 1 to 4 bytes.
 *   covered  each series' covered ranges
 * The layout's version is the file's `user_version`. A file of the layout before it (version 0: the name in every
 * row, 8-byte prices) is converted in place when opened; one of a newer layout is left alone. The compact tables keep
 * the old names but not the old columns, so a plugin from before the change fails to read or write them rather than
 * using empty tables. WAL journal, so a flush does not stall readers.
 */
use super::{Change, Key, Series};
use crate::Bar;
use rusqlite::types::Value;
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub struct Sqlite(Arc<Mutex<Connection>>);

/// The compact layout's version (`PRAGMA user_version`).
pub const LAYOUT: i64 = 1;

/// How long an open waits for another process holding the file, a conversion included.
const WAIT: Duration = Duration::from_secs(120);

/// The most decimals a price is stored with as a whole number.
const MAX_DECIMALS: i32 = 9;

const PRAGMAS: &str = "PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;";

/// The compact layout's tables, `bars` and `covered` under the given names.
fn schema(bars: &str, covered: &str) -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS series (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE);
         CREATE TABLE IF NOT EXISTS {bars} (s INTEGER NOT NULL, t INTEGER NOT NULL, o, h, l, c, v, x, PRIMARY KEY (s, t)) WITHOUT ROWID;
         CREATE TABLE IF NOT EXISTS {covered} (s INTEGER NOT NULL, a INTEGER NOT NULL, b INTEGER NOT NULL, PRIMARY KEY (s, a)) WITHOUT ROWID;"
    )
}

/// What converting a file of the old layout moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Converted {
    pub series: usize,
    pub bars: usize,
}

/// Open, high, low and close as whole numbers of 10^-x, with the smallest such x, when each comes back bit for bit.
pub fn steps(prices: [f64; 4]) -> Option<(i64, [i64; 4])> {
    'scale: for x in 0..=MAX_DECIMALS {
        let mut out = [0i64; 4];
        for (n, p) in out.iter_mut().zip(prices) {
            let whole = (p * 10f64.powi(x)).round();
            if !whole.is_finite() || whole.abs() >= 9_007_199_254_740_992.0 || price_of(whole as i64, x as i64).to_bits() != p.to_bits() {
                continue 'scale;
            }
            *n = whole as i64;
        }
        return Some((x as i64, out));
    }
    None
}

/// A price stored as `n` whole steps of 10^-x.
pub fn price_of(n: i64, x: i64) -> f64 {
    n as f64 / 10f64.powi(x as i32)
}

/// A volume as stored: a whole number when it is one (and comes back bit for bit), else a decimal.
fn volume_value(v: f64) -> Value {
    if v.fract() == 0.0 && v.abs() < 9_007_199_254_740_992.0 && ((v as i64) as f64).to_bits() == v.to_bits() {
        Value::Integer(v as i64)
    } else {
        Value::Real(v)
    }
}

fn number(v: Value) -> rusqlite::Result<f64> {
    match v {
        Value::Integer(n) => Ok(n as f64),
        Value::Real(r) => Ok(r),
        other => Err(rusqlite::Error::InvalidColumnType(0, format!("{other:?}"), rusqlite::types::Type::Real)),
    }
}

/// Writes one bar of series `s` with `ins` (`INSERT OR REPLACE INTO bars VALUES (s, t, o, h, l, c, v, x)`).
fn insert_bar(ins: &mut rusqlite::CachedStatement<'_>, s: i64, b: &Bar) -> rusqlite::Result<()> {
    let prices = [b[1], b[2], b[3], b[4]];
    let (p, x): ([Value; 4], Value) = match steps(prices) {
        Some((x, n)) => (n.map(Value::Integer), Value::Integer(x)),
        None => (prices.map(Value::Real), Value::Null),
    };
    let [o, h, l, c] = p;
    ins.execute(params![s, b[0] as i64, o, h, l, c, volume_value(b[5]), x])?;
    Ok(())
}

/// A stored bar back: `t, o, h, l, c, v, x`.
fn bar_of(r: &rusqlite::Row<'_>) -> rusqlite::Result<Bar> {
    let t: i64 = r.get(0)?;
    let x: Option<i64> = r.get(6)?;
    let mut bar = [t as f64, 0.0, 0.0, 0.0, 0.0, number(r.get(5)?)?];
    for (i, price) in bar.iter_mut().enumerate().take(5).skip(1) {
        *price = match (x, r.get::<_, Value>(i)?) {
            (Some(x), Value::Integer(n)) => price_of(n, x),
            (_, v) => number(v)?,
        };
    }
    Ok(bar)
}

fn user_version(c: &Connection) -> rusqlite::Result<i64> {
    c.query_row("PRAGMA user_version", [], |r| r.get(0))
}

fn newer(path: &Path, v: i64) -> String {
    format!(
        "the bar store {} is of a newer layout ({v}) than this plugin knows ({LAYOUT}): update the plugin. The file is left as it is",
        path.display()
    )
}

/// Brings the file to the compact layout inside `tx`: creates it in a new file, converts the old layout. Whether a
/// conversion ran, and what it moved.
fn to_compact(tx: &Transaction<'_>, path: &Path) -> Result<Option<Converted>, String> {
    let e = |e: rusqlite::Error| e.to_string();
    match user_version(tx).map_err(e)? {
        LAYOUT => return Ok(None),
        0 => {}
        v => return Err(newer(path, v)),
    }
    let has_bars: bool = tx.query_row("SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'bars'", [], |r| r.get::<_, i64>(0)).map_err(e)? > 0;
    if !has_bars {
        tx.execute_batch(&schema("bars", "covered")).map_err(e)?;
        tx.pragma_update(None, "user_version", LAYOUT).map_err(e)?;
        return Ok(None);
    }
    let old: bool = tx.query_row("SELECT count(*) FROM pragma_table_info('bars') WHERE name = 'series'", [], |r| r.get::<_, i64>(0)).map_err(e)? > 0;
    if !old {
        return Err(format!("the bar store {} is of an unknown layout", path.display()));
    }
    // The old covered table may be missing in a hand-made file.
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS covered (series TEXT NOT NULL, a INTEGER NOT NULL, b INTEGER NOT NULL, PRIMARY KEY (series, a)) WITHOUT ROWID;",
    )
    .map_err(e)?;
    tx.execute_batch(&schema("bars_compact", "covered_compact")).map_err(e)?;
    let series = tx
        .execute("INSERT INTO series (name) SELECT series FROM bars UNION SELECT series FROM covered ORDER BY 1", [])
        .map_err(e)?;
    let mut moved = 0;
    {
        let mut read = tx
            .prepare("SELECT s.id, b.t, b.open, b.high, b.low, b.close, b.volume FROM bars b JOIN series s ON s.name = b.series ORDER BY s.id, b.t")
            .map_err(e)?;
        let mut ins = tx.prepare_cached("INSERT OR REPLACE INTO bars_compact VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)").map_err(e)?;
        let mut rows = read.query([]).map_err(e)?;
        while let Some(r) = rows.next().map_err(e)? {
            let s: i64 = r.get(0).map_err(e)?;
            let t: i64 = r.get(1).map_err(e)?;
            let b: Bar = [t as f64, r.get(2).map_err(e)?, r.get(3).map_err(e)?, r.get(4).map_err(e)?, r.get(5).map_err(e)?, r.get(6).map_err(e)?];
            insert_bar(&mut ins, s, &b).map_err(e)?;
            moved += 1;
        }
    }
    tx.execute_batch(
        "INSERT INTO covered_compact SELECT s.id, c.a, c.b FROM covered c JOIN series s ON s.name = c.series;
         DROP TABLE bars; DROP TABLE covered;
         ALTER TABLE bars_compact RENAME TO bars; ALTER TABLE covered_compact RENAME TO covered;",
    )
    .map_err(e)?;
    tx.pragma_update(None, "user_version", LAYOUT).map_err(e)?;
    Ok(Some(Converted { series, bars: moved }))
}

impl Sqlite {
    pub fn open(path: &Path) -> Result<Self, String> {
        Self::open_converting(path).map(|(s, _)| s)
    }

    /// Opens the file at `path`, converting a file of the old layout first (in one transaction: whole or not at
    /// all). A file of a newer layout is refused and left as it is. The conversion it ran, if any.
    pub fn open_converting(path: &Path) -> Result<(Self, Option<Converted>), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let mut conn = Connection::open(path).map_err(|e| e.to_string())?;
        // The plugin and a backfill run may write the same file: one waits for the other, a conversion included.
        conn.busy_timeout(WAIT).map_err(|e| e.to_string())?;
        // Read before anything is written: a newer layout's file must stay as it is.
        match user_version(&conn).map_err(|e| e.to_string())? {
            v if v > LAYOUT => return Err(newer(path, v)),
            _ => {}
        }
        conn.execute_batch(PRAGMAS).map_err(|e| e.to_string())?;
        let started = Instant::now();
        let converted = {
            // Immediate: a second process opening the file waits here until the first has converted it.
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e| e.to_string())?;
            let converted = to_compact(&tx, path).map_err(|e| format!("the bar store could not be converted to the compact layout: {e}"))?;
            tx.commit().map_err(|e| e.to_string())?;
            converted
        };
        if let Some(c) = converted {
            // Gives the old layout's space back: the file shrinks to the compact size.
            let shrunk = conn.execute_batch("VACUUM; PRAGMA wal_checkpoint(TRUNCATE);");
            println!(
                "[info] the bar store was converted to the compact layout: {} series, {} bars in {:.1} s",
                c.series,
                c.bars,
                started.elapsed().as_secs_f64()
            );
            if let Err(e) = shrunk {
                println!("[warn] the converted bar store could not be compacted ({e}); it is compacted at the next conversion");
            }
        }
        Ok((Sqlite(Arc::new(Mutex::new(conn))), converted))
    }

    /// In memory (tests).
    pub fn memory() -> Self {
        let conn = Connection::open_in_memory().expect("an in-memory database");
        conn.execute_batch(&schema("bars", "covered")).expect("the schema");
        conn.pragma_update(None, "user_version", LAYOUT).expect("the layout version");
        Sqlite(Arc::new(Mutex::new(conn)))
    }

    async fn run<R: Send + 'static>(&self, f: impl FnOnce(&mut Connection) -> rusqlite::Result<R> + Send + 'static) -> Result<R, String> {
        let conn = self.0.clone();
        tokio::task::spawn_blocking(move || f(&mut conn.lock().unwrap_or_else(|e| e.into_inner())))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())
    }

    pub async fn load(&self, key: &Key) -> Result<Series, String> {
        let name = key.id();
        self.run(move |c| {
            let Some(s) = c.query_row("SELECT id FROM series WHERE name = ?1", [&name], |r| r.get::<_, i64>(0)).optional()? else {
                return Ok(Series::default());
            };
            let mut q = c.prepare_cached("SELECT t, o, h, l, c, v, x FROM bars WHERE s = ?1 ORDER BY t")?;
            let bars = q.query_map([s], bar_of)?.collect::<rusqlite::Result<Vec<Bar>>>()?;
            let mut q = c.prepare_cached("SELECT a, b FROM covered WHERE s = ?1 ORDER BY a")?;
            let covered = q.query_map([s], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<Vec<(i64, i64)>>>()?;
            Ok(Series::loaded(bars, covered))
        })
        .await
    }

    pub async fn save(&self, key: &Key, change: &Change) -> Result<(), String> {
        let name = key.id();
        let change = change.clone();
        self.run(move |c| {
            let tx = c.transaction()?;
            tx.execute("INSERT OR IGNORE INTO series (name) VALUES (?1)", [&name])?;
            let s: i64 = tx.query_row("SELECT id FROM series WHERE name = ?1", [&name], |r| r.get(0))?;
            if change.cleared {
                tx.execute("DELETE FROM bars WHERE s = ?1", [s])?;
            }
            {
                let mut ins = tx.prepare_cached("INSERT OR REPLACE INTO bars VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)")?;
                for b in &change.added {
                    insert_bar(&mut ins, s, b)?;
                }
            }
            tx.execute("DELETE FROM covered WHERE s = ?1", [s])?;
            {
                let mut ins = tx.prepare_cached("INSERT INTO covered VALUES (?1, ?2, ?3)")?;
                for (a, b) in &change.covered {
                    ins.execute(params![s, a, b])?;
                }
            }
            tx.commit()
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::bar;
    use super::super::{Backend, Store};
    use super::*;

    /// The old layout's store (plugin kit 0.1.0, moomoo 0.2.4), as it wrote and read the file.
    mod old {
        use crate::Bar;
        use rusqlite::{params, Connection};

        pub const SCHEMA: &str = "
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = NORMAL;
            CREATE TABLE IF NOT EXISTS bars (
                series TEXT NOT NULL, t INTEGER NOT NULL,
                open REAL NOT NULL, high REAL NOT NULL, low REAL NOT NULL, close REAL NOT NULL, volume REAL NOT NULL,
                PRIMARY KEY (series, t)
            ) WITHOUT ROWID;
            CREATE TABLE IF NOT EXISTS covered (
                series TEXT NOT NULL, a INTEGER NOT NULL, b INTEGER NOT NULL,
                PRIMARY KEY (series, a)
            ) WITHOUT ROWID;
        ";

        pub fn open(path: &std::path::Path) -> rusqlite::Result<Connection> {
            let c = Connection::open(path)?;
            c.execute_batch(SCHEMA)?;
            Ok(c)
        }

        pub fn save(c: &mut Connection, id: &str, bars: &[Bar], covered: &[(i64, i64)]) -> rusqlite::Result<()> {
            let tx = c.transaction()?;
            {
                let mut ins = tx.prepare_cached("INSERT OR REPLACE INTO bars VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)")?;
                for b in bars {
                    ins.execute(params![id, b[0] as i64, b[1], b[2], b[3], b[4], b[5]])?;
                }
            }
            tx.execute("DELETE FROM covered WHERE series = ?1", [id])?;
            {
                let mut ins = tx.prepare_cached("INSERT INTO covered VALUES (?1, ?2, ?3)")?;
                for (a, b) in covered {
                    ins.execute(params![id, a, b])?;
                }
            }
            tx.commit()
        }

        /// A series' bars and covered ranges.
        pub type Stored = (Vec<Bar>, Vec<(i64, i64)>);

        pub fn load(c: &Connection, id: &str) -> rusqlite::Result<Stored> {
            let mut q = c.prepare_cached("SELECT t, open, high, low, close, volume FROM bars WHERE series = ?1 ORDER BY t")?;
            let bars = q
                .query_map([id], |r| Ok([r.get::<_, i64>(0)? as f64, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?]))?
                .collect::<rusqlite::Result<Vec<Bar>>>()?;
            let mut q = c.prepare_cached("SELECT a, b FROM covered WHERE series = ?1 ORDER BY a")?;
            let covered = q.query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<Vec<(i64, i64)>>>()?;
            Ok((bars, covered))
        }
    }

    /// A file path of its own for test `name`; removed with its journal files when dropped.
    struct TempFile(std::path::PathBuf);

    impl TempFile {
        fn new(name: &str) -> Self {
            let f = TempFile(std::env::temp_dir().join(format!("ot-kit-{name}-{}.sqlite", std::process::id())));
            f.remove();
            f
        }

        fn remove(&self) {
            for ext in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{ext}", self.0.display()));
            }
        }

        /// The file's size once it has no free pages and its journal is folded in.
        fn compact_size(&self) -> u64 {
            let c = Connection::open(&self.0).unwrap();
            c.execute_batch("VACUUM; PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
            drop(c);
            std::fs::metadata(&self.0).unwrap().len()
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            self.remove();
        }
    }

    fn bits(bars: &[Bar]) -> Vec<[u64; 6]> {
        bars.iter().map(|b| b.map(f64::to_bits)).collect()
    }

    /// `n` 1-minute bars from 2023-11-14 with prices on a 0.01 step, as a broker's "150.25" parses: the low, open,
    /// close and high `cents(i)` and 2, 7 and 10 cents above it.
    fn minutes(n: usize, cents: impl Fn(usize) -> i64) -> Vec<Bar> {
        (0..n).map(|i| {
            let c = cents(i);
            let p = |d: i64| (c + d) as f64 / 100.0;
            [(1_700_000_040 + 60 * i as i64) as f64, p(2), p(10), p(0), p(7), (1000 + i % 5000) as f64]
        })
        .collect()
    }

    /// `n` 1-minute bars with prices off any step, as after a split adjustment: `price(i)` and around it.
    fn adjusted(n: usize, price: impl Fn(usize) -> f64) -> Vec<Bar> {
        (0..n).map(|i| {
            let p = price(i);
            [(1_700_000_040 + 60 * i as i64) as f64, p, p * 1.000_312_345_679, p * 0.999_712_345_679, p * 1.000_101_234_568, (1000 + i % 5000) as f64]
        })
        .collect()
    }

    /// A series of an old file: its name, bars and covered ranges.
    type OldSeries<'a> = (&'a str, &'a [Bar], &'a [(i64, i64)]);

    /// The old layout's file at `path` holding `series` with their bars and covered ranges.
    fn old_file(path: &Path, series: &[OldSeries<'_>]) {
        let mut c = old::open(path).unwrap();
        for (id, bars, covered) in series {
            old::save(&mut c, id, bars, covered).unwrap();
        }
    }

    #[tokio::test]
    async fn a_flushed_series_is_read_back_by_a_new_store() {
        let file = TempFile::new("flushed");
        let path = &file.0;
        let key = Key::new("US:AAPL", "1", true);
        {
            let store = Store::new(Backend::Sqlite(Sqlite::open(path).unwrap()));
            store.update(&key, |s| {
                s.merge(&[bar(60, 1.0), bar(120, 2.0)]);
                s.cover(0, 179);
            }).await;
            store.flush().await;
            store.update(&key, |s| s.merge(&[bar(120, 2.5)])).await;
            store.flush().await;
        }
        let store = Store::new(Backend::Sqlite(Sqlite::open(path).unwrap()));
        let (closes, missing) = store.read(&key, |s| (s.window(0, 200).iter().map(|b| b[4]).collect::<Vec<_>>(), s.missing(0, 179))).await;
        assert_eq!((closes, missing), (vec![1.0, 2.5], vec![]));
    }

    #[tokio::test]
    async fn a_dropped_series_is_deleted() {
        let store = Store::new(Backend::Sqlite(Sqlite::memory()));
        let key = Key::new("US:AAPL", "1D", false);
        store.update(&key, |s| s.merge(&[bar(0, 1.0)])).await;
        store.flush().await;
        store.update(&key, |s| {
            s.clear();
            s.merge(&[bar(86_400, 2.0)]);
        }).await;
        store.flush().await;
        let Backend::Sqlite(db) = &**store.backend.read().await else { unreachable!() };
        let s = db.load(&key).await.unwrap();
        assert_eq!(s.window(0, i64::MAX).iter().map(|b| b[4]).collect::<Vec<_>>(), [2.0]);
    }

    #[test]
    fn prices_are_whole_steps_only_when_they_come_back_exactly() {
        assert_eq!(steps([150.25, 150.3, 150.0, 150.05]), Some((2, [15025, 15030, 15000, 15005])));
        assert_eq!(steps([2.0, 3.0, 1.0, 2.0]), Some((0, [2, 3, 1, 2])));
        let (x, n) = steps([150.123456789, 150.2, 150.1, 150.15]).unwrap();
        assert_eq!(price_of(n[0], x).to_bits(), 150.123456789f64.to_bits());
        // Off any step (as after a split adjustment), and -0.0: kept as decimals.
        assert_eq!(steps([150.12345678912345, 1.0, 1.0, 1.0]), None);
        assert_eq!(steps([-0.0, 1.0, 1.0, 1.0]), None);
        assert_eq!(steps([f64::NAN, 1.0, 1.0, 1.0]), None);
    }

    /// REQ-BAR-STORE-001.A1–A3: bars come back bit for bit, covered ranges as written, series apart.
    #[tokio::test]
    async fn bars_come_back_exactly_as_written() {
        let file = TempFile::new("exact");
        let on_step: Bar = [60.0, 150.25, 150.5, 150.0, 150.3, 1200.0];
        let off_step: Bar = [120.0, 150.123456789, 150.12345678912345, 149.98765432109876, 1e-7, 0.5];
        let big: Bar = [180.0, 98_765.432_1, 1e15, 0.0001, 12_345_678.9, 1e18];
        let keys = [Key::new("US:AAPL", "1", true), Key::new("US:AAPL", "1", false), Key::new("US:AAPL", "5", true)];
        {
            let store = Store::new(Backend::Sqlite(Sqlite::open(&file.0).unwrap()));
            store.update(&keys[0], |s| {
                s.merge(&[on_step, off_step, big]);
                s.cover(0, 239);
                s.cover(600, 899);
            }).await;
            store.update(&keys[1], |s| s.merge(&[[60.0, 1.0, 1.0, 1.0, 1.0, 1.0]])).await;
            store.update(&keys[2], |s| {
                s.merge(&[[0.0, 7.0, 7.0, 7.0, 7.0, 7.0]]);
                s.cover(0, 299);
            }).await;
            store.flush().await;
            // Dropped and filled again (adjusted prices changed).
            store.update(&keys[2], |s| {
                s.clear();
                s.merge(&[[300.0, 8.5, 8.5, 8.5, 8.5, 8.0]]);
                s.cover(300, 599);
            }).await;
            store.flush().await;
        }
        let db = Sqlite::open(&file.0).unwrap();
        let s = db.load(&keys[0]).await.unwrap();
        assert_eq!(bits(&s.window(0, i64::MAX)), bits(&[on_step, off_step, big]));
        assert_eq!(s.covered(), [(0, 239), (600, 899)]);
        assert_eq!(bits(&db.load(&keys[1]).await.unwrap().window(0, i64::MAX)), bits(&[[60.0, 1.0, 1.0, 1.0, 1.0, 1.0]]));
        let s = db.load(&keys[2]).await.unwrap();
        assert_eq!(bits(&s.window(0, i64::MAX)), bits(&[[300.0, 8.5, 8.5, 8.5, 8.5, 8.0]]));
        assert_eq!(s.covered(), [(300, 599)]);
        assert_eq!(db.load(&Key::new("US:MSFT", "1", true)).await.unwrap(), Series::default());
    }

    /// REQ-BAR-STORE-002 and REQ-BAR-STORE-003.A5: half the old size or less on a price step, no larger off it.
    #[test]
    fn the_compact_file_takes_half_the_space_or_less() {
        for (name, bars, most) in [
            ("on-step", minutes(400_000, |i| 15_000 + (i % 997) as i64), 0.5),
            ("off-step", adjusted(400_000, |i| 150.0 + (i % 997) as f64 * 0.012_345_678_912), 1.0),
        ] {
            let file = TempFile::new(&format!("size-{name}"));
            old_file(&file.0, &[("US:AAPL|1|ext", &bars, &[(0, 1)])]);
            let before = file.compact_size();
            let (_, converted) = Sqlite::open_converting(&file.0).unwrap();
            assert_eq!(converted, Some(Converted { series: 1, bars: 400_000 }));
            let after = file.compact_size();
            assert!((after as f64) <= before as f64 * most, "{name}: {after} bytes against {before} before");
            // Without compacting it again, the conversion already gave the space back.
            assert!(std::fs::metadata(&file.0).unwrap().len() <= after + 4096 * 8, "{name}: the file did not shrink");
        }
    }

    /// REQ-BAR-STORE-003.A1: an old file's series all come back after the conversion as the old store read them.
    #[tokio::test]
    async fn an_old_file_is_converted_in_place() {
        let file = TempFile::new("convert");
        let aapl = minutes(500, |i| 15_000 + i as i64);
        let adjusted = adjusted(30, |i| 75.123456789123 + i as f64 / 3.0);
        let series: [OldSeries<'_>; 3] = [
            ("US:AAPL|1|ext", &aapl, &[(1_700_000_000, 1_700_030_039)]),
            ("US:AAPL|1D|rth", &adjusted, &[(0, 86_399), (172_800, 259_199)]),
            ("NSE:RELIANCE|5|rth", &[], &[(0, 299)]),
        ];
        old_file(&file.0, &series);
        let was: Vec<_> = {
            let c = old::open(&file.0).unwrap();
            series.iter().map(|(id, ..)| old::load(&c, id).unwrap()).collect()
        };
        let (db, converted) = Sqlite::open_converting(&file.0).unwrap();
        assert_eq!(converted, Some(Converted { series: 3, bars: 530 }));
        for ((id, ..), (bars, covered)) in series.iter().zip(&was) {
            let (sym, rest) = id.split_once('|').unwrap();
            let (res, session) = rest.split_once('|').unwrap();
            let s = db.load(&Key::new(sym, res, session == "ext")).await.unwrap();
            assert_eq!(bits(&s.window(0, i64::MAX)), bits(bars), "{id}");
            assert_eq!(s.covered(), covered.as_slice(), "{id}");
        }
        // Opened again: nothing left to convert.
        drop(db);
        assert_eq!(Sqlite::open_converting(&file.0).unwrap().1, None);
    }

    /// REQ-BAR-STORE-003.A2: a conversion stopped part way leaves the old file as it was; the next open converts it.
    #[test]
    fn a_failed_conversion_leaves_the_old_file() {
        let file = TempFile::new("failed");
        let bars = minutes(200, |i| 1_000 + 50 * i as i64);
        old_file(&file.0, &[("US:AAPL|1|ext", &bars, &[(5, 9)])]);
        // A table in the way of the conversion's own makes it fail after it has started writing.
        Connection::open(&file.0).unwrap().execute_batch("CREATE TABLE bars_compact (x);").unwrap();
        let err = Sqlite::open_converting(&file.0).err().expect("the conversion fails");
        assert!(err.contains("could not be converted"), "{err}");
        let c = old::open(&file.0).unwrap();
        assert_eq!(old::load(&c, "US:AAPL|1|ext").unwrap(), (bars.clone(), vec![(5, 9)]));
        assert_eq!(user_version(&c).unwrap(), 0);
        let no_series: i64 = c.query_row("SELECT count(*) FROM sqlite_master WHERE name = 'series'", [], |r| r.get(0)).unwrap();
        assert_eq!(no_series, 0);
        c.execute_batch("DROP TABLE bars_compact;").unwrap();
        drop(c);
        assert_eq!(Sqlite::open_converting(&file.0).unwrap().1, Some(Converted { series: 1, bars: 200 }));
    }

    /// REQ-BAR-STORE-003.A4: two processes opening one old file at once convert it once, and read the same bars.
    #[tokio::test]
    async fn two_openers_convert_once() {
        let file = TempFile::new("twice");
        let bars = minutes(20_000, |i| 10_000 + 25 * (i % 50) as i64);
        old_file(&file.0, &[("US:AAPL|1|ext", &bars, &[])]);
        let opened: Vec<_> = (0..2)
            .map(|_| {
                let path = file.0.clone();
                std::thread::spawn(move || Sqlite::open_converting(&path).unwrap())
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|t| t.join().unwrap())
            .collect();
        assert_eq!(opened.iter().filter(|(_, c)| c.is_some()).count(), 1);
        let key = Key::new("US:AAPL", "1", true);
        for (db, _) in &opened {
            assert_eq!(bits(&db.load(&key).await.unwrap().window(0, i64::MAX)), bits(&bars));
        }
    }

    /// REQ-BAR-STORE-003.A6: a million bars convert in under 30 s.
    #[test]
    fn a_million_bars_convert_in_under_30_seconds() {
        let file = TempFile::new("million");
        let a = minutes(500_000, |i| 15_000 + (i % 997) as i64);
        let b = adjusted(500_000, |i| 75.0 + (i % 991) as f64 * 0.05);
        old_file(&file.0, &[("US:AAPL|1|ext", &a, &[]), ("US:MSFT|1|ext", &b, &[])]);
        let started = Instant::now();
        let (_, converted) = Sqlite::open_converting(&file.0).unwrap();
        let took = started.elapsed();
        assert_eq!(converted, Some(Converted { series: 2, bars: 1_000_000 }));
        assert!(took < Duration::from_secs(30), "{took:?}");
    }

    /// REQ-BAR-STORE-004.A1: a file of a newer layout is refused and left byte for byte as it was.
    #[test]
    fn a_newer_layout_is_left_alone() {
        let file = TempFile::new("newer");
        drop(Sqlite::open(&file.0).unwrap());
        let c = Connection::open(&file.0).unwrap();
        c.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode = DELETE;").unwrap();
        c.pragma_update(None, "user_version", LAYOUT + 1).unwrap();
        drop(c);
        let before = std::fs::read(&file.0).unwrap();
        let err = Sqlite::open(&file.0).err().expect("a newer layout is refused");
        assert!(err.contains("update the plugin"), "{err}");
        assert_eq!(std::fs::read(&file.0).unwrap(), before);
    }

    /// REQ-BAR-STORE-004.A2: the old store fails to read and to write a converted file, and leaves its bars.
    #[tokio::test]
    async fn the_old_store_cannot_use_a_converted_file() {
        let file = TempFile::new("old-store");
        let bars = minutes(10, |i| 2_000 + 100 * i as i64);
        old_file(&file.0, &[("US:AAPL|1|ext", &bars, &[(0, 599)])]);
        drop(Sqlite::open(&file.0).unwrap());
        let mut c = old::open(&file.0).unwrap();
        assert!(old::load(&c, "US:AAPL|1|ext").is_err());
        assert!(old::save(&mut c, "US:AAPL|1|ext", &[bar(0, 1.0)], &[(0, 59)]).is_err());
        assert!(old::save(&mut c, "US:MSFT|1|ext", &[], &[(0, 59)]).is_err());
        drop(c);
        let s = Sqlite::open(&file.0).unwrap().load(&Key::new("US:AAPL", "1", true)).await.unwrap();
        assert_eq!(bits(&s.window(0, i64::MAX)), bits(&bars));
        assert_eq!(s.covered(), [(0, 599)]);
    }
}
