/*
 * The embedded store: one SQLite file. `bars` is clustered on (series, t) (WITHOUT ROWID), so a series is one range
 * scan of the primary key; `covered` holds each series' ranges. WAL journal, so a flush does not stall readers.
 */
use super::{Change, Key, Series};
use crate::Bar;
use rusqlite::{params, Connection};
use std::path::Path;
use std::sync::{Arc, Mutex};

pub struct Sqlite(Arc<Mutex<Connection>>);

const SCHEMA: &str = "
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

impl Sqlite {
    pub fn open(path: &Path) -> Result<Self, String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let conn = Connection::open(path).map_err(|e| e.to_string())?;
        // The plugin and a backfill run may write the same file: one waits for the other.
        conn.busy_timeout(std::time::Duration::from_secs(30)).map_err(|e| e.to_string())?;
        conn.execute_batch(SCHEMA).map_err(|e| e.to_string())?;
        Ok(Sqlite(Arc::new(Mutex::new(conn))))
    }

    /// In memory (tests).
    pub fn memory() -> Self {
        let conn = Connection::open_in_memory().expect("an in-memory database");
        conn.execute_batch(SCHEMA).expect("the schema");
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
        let id = key.id();
        self.run(move |c| {
            let mut q = c.prepare_cached("SELECT t, open, high, low, close, volume FROM bars WHERE series = ?1 ORDER BY t")?;
            let bars = q
                .query_map([&id], |r| Ok([r.get::<_, i64>(0)? as f64, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?]))?
                .collect::<rusqlite::Result<Vec<Bar>>>()?;
            let mut q = c.prepare_cached("SELECT a, b FROM covered WHERE series = ?1 ORDER BY a")?;
            let covered = q.query_map([&id], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<Vec<(i64, i64)>>>()?;
            Ok(Series::loaded(bars, covered))
        })
        .await
    }

    pub async fn save(&self, key: &Key, change: &Change) -> Result<(), String> {
        let id = key.id();
        let change = change.clone();
        self.run(move |c| {
            let tx = c.transaction()?;
            if change.cleared {
                tx.execute("DELETE FROM bars WHERE series = ?1", [&id])?;
            }
            {
                let mut ins = tx.prepare_cached("INSERT OR REPLACE INTO bars VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)")?;
                for b in &change.added {
                    ins.execute(params![id, b[0] as i64, b[1], b[2], b[3], b[4], b[5]])?;
                }
            }
            tx.execute("DELETE FROM covered WHERE series = ?1", [&id])?;
            {
                let mut ins = tx.prepare_cached("INSERT INTO covered VALUES (?1, ?2, ?3)")?;
                for (a, b) in &change.covered {
                    ins.execute(params![id, a, b])?;
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

    #[tokio::test]
    async fn a_flushed_series_is_read_back_by_a_new_store() {
        let path = std::env::temp_dir().join(format!("ot-moomoo-{}.sqlite", std::process::id()));
        let key = Key::new("US:AAPL", "1", true);
        {
            let store = Store::new(Backend::Sqlite(Sqlite::open(&path).unwrap()));
            store.update(&key, |s| {
                s.merge(&[bar(60, 1.0), bar(120, 2.0)]);
                s.cover(0, 179);
            }).await;
            store.flush().await;
            store.update(&key, |s| s.merge(&[bar(120, 2.5)])).await;
            store.flush().await;
        }
        let store = Store::new(Backend::Sqlite(Sqlite::open(&path).unwrap()));
        let (closes, missing) = store.read(&key, |s| (s.window(0, 200).iter().map(|b| b[4]).collect::<Vec<_>>(), s.missing(0, 179))).await;
        assert_eq!((closes, missing), (vec![1.0, 2.5], vec![]));
        for ext in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{ext}", path.display()));
        }
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
}
