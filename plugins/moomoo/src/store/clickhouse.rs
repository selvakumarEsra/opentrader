/*
 * The ClickHouse store, over ClickHouse's HTTP interface (default http://localhost:8123). Two tables in the user's
 * database, made if missing:
 *
 *   moomoo_bars     ReplacingMergeTree(version), PARTITION BY month, ORDER BY (symbol, exchange, resolution,
 *                   session, timestamp): a series is one range of the sort key, and a bar written again replaces
 *                   the earlier one (read with FINAL).
 *   moomoo_covered  ReplacingMergeTree(version), ORDER BY series: each series' covered ranges, as JSON.
 *
 * Values travel as query parameters ({name:Type} with param_name), never spliced into SQL.
 */
use super::{Change, Key, Series};
use crate::bars::Bar;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone)]
pub struct ClickHouse(Arc<Inner>);

struct Inner {
    url: String,
    database: String,
    user: String,
    password: String,
    agent: ureq::Agent,
    ready: AtomicBool,
    version: AtomicU64,
}

/// A database name ClickHouse takes unquoted.
fn valid_name(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') && !s.starts_with(|c: char| c.is_ascii_digit())
}

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

impl ClickHouse {
    pub fn new(url: &str, database: &str, user: &str, password: &str) -> Result<Self, String> {
        let url = url.trim().trim_end_matches('/').to_string();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err(format!("{url:?} is not a ClickHouse HTTP address (http://host:8123)"));
        }
        let database = database.trim().to_string();
        if !valid_name(&database) {
            return Err(format!("{database:?} is not a database name (letters, digits and _)"));
        }
        let agent: ureq::Agent =
            ureq::Agent::config_builder().http_status_as_error(false).timeout_global(Some(Duration::from_secs(60))).build().into();
        Ok(ClickHouse(Arc::new(Inner {
            url,
            database,
            user: user.trim().to_string(),
            password: password.to_string(),
            agent,
            ready: AtomicBool::new(false),
            version: AtomicU64::new(0),
        })))
    }

    /// One statement; `params` fill its {name:Type} placeholders; `body` is the INSERT's data.
    fn query(&self, sql: &str, params: &[(&str, &str)], body: Option<String>) -> Result<String, String> {
        let i = &self.0;
        let mut url = format!("{}/?query={}&output_format_json_quote_64bit_integers=0", i.url, encode(sql));
        for (k, v) in params {
            url.push_str(&format!("&param_{k}={}", encode(v)));
        }
        let mut req = i.agent.post(&url);
        if !i.user.is_empty() {
            req = req.header("X-ClickHouse-User", &i.user).header("X-ClickHouse-Key", &i.password);
        }
        let resp = req.send(body.unwrap_or_default()).map_err(|e| format!("ClickHouse at {} is not reachable: {e}", i.url))?;
        let status = resp.status();
        let text = resp.into_body().into_with_config().limit(u64::MAX).read_to_string().map_err(|e| e.to_string())?;
        if !status.is_success() {
            return Err(format!("ClickHouse refused: {}", text.lines().next().unwrap_or("").trim()));
        }
        Ok(text)
    }

    fn ensure_tables(&self) -> Result<(), String> {
        if self.0.ready.load(Ordering::Relaxed) {
            return Ok(());
        }
        let db = &self.0.database;
        self.query(&format!("CREATE DATABASE IF NOT EXISTS {db}"), &[], None)?;
        self.query(
            &format!(
                "CREATE TABLE IF NOT EXISTS {db}.moomoo_bars (
                    symbol String, exchange LowCardinality(String), resolution LowCardinality(String),
                    session LowCardinality(String), timestamp DateTime64(3, 'UTC'),
                    open Float64, high Float64, low Float64, close Float64, volume Float64, version UInt64
                ) ENGINE = ReplacingMergeTree(version)
                PARTITION BY toYYYYMM(timestamp)
                ORDER BY (symbol, exchange, resolution, session, timestamp)"
            ),
            &[],
            None,
        )?;
        self.query(
            &format!(
                "CREATE TABLE IF NOT EXISTS {db}.moomoo_covered (series String, covered String, version UInt64)
                ENGINE = ReplacingMergeTree(version) ORDER BY series"
            ),
            &[],
            None,
        )?;
        self.0.ready.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// Increasing write versions, so the newest write of a bar wins FINAL.
    fn next_version(&self) -> u64 {
        let now = chrono::Utc::now().timestamp_millis().max(0) as u64;
        let prev = self.0.version.fetch_max(now, Ordering::Relaxed);
        if prev >= now {
            self.0.version.fetch_add(1, Ordering::Relaxed) + 1
        } else {
            now
        }
    }

    async fn blocking<R: Send + 'static>(&self, f: impl FnOnce(&ClickHouse) -> Result<R, String> + Send + 'static) -> Result<R, String> {
        let me = self.clone();
        tokio::task::spawn_blocking(move || f(&me)).await.map_err(|e| e.to_string())?
    }

    pub async fn load(&self, key: &Key) -> Result<Series, String> {
        let key = key.clone();
        self.blocking(move |c| {
            c.ensure_tables()?;
            let db = &c.0.database;
            let (exchange, symbol) = key.parts();
            let p = [("s", symbol), ("e", exchange), ("r", key.resolution.as_str()), ("x", key.session())];
            let rows = c.query(
                &format!(
                    "SELECT toUnixTimestamp64Milli(timestamp), open, high, low, close, volume FROM {db}.moomoo_bars FINAL
                     WHERE symbol = {{s:String}} AND exchange = {{e:String}} AND resolution = {{r:String}} AND session = {{x:String}}
                     ORDER BY timestamp FORMAT JSONCompactEachRow"
                ),
                &p,
                None,
            )?;
            let bars: Vec<Bar> = rows
                .lines()
                .filter_map(|l| {
                    let v: Vec<f64> = serde_json::from_str(l).ok()?;
                    (v.len() == 6).then(|| [(v[0] / 1000.0).floor(), v[1], v[2], v[3], v[4], v[5]])
                })
                .collect();
            let id = key.id();
            let covered = c.query(
                &format!("SELECT covered FROM {db}.moomoo_covered FINAL WHERE series = {{id:String}} FORMAT JSONCompactEachRow"),
                &[("id", id.as_str())],
                None,
            )?;
            let covered: Vec<(i64, i64)> = covered
                .lines()
                .next()
                .and_then(|l| serde_json::from_str::<Vec<String>>(l).ok())
                .and_then(|row| row.into_iter().next())
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
            Ok(Series::loaded(bars, covered))
        })
        .await
    }

    pub async fn save(&self, key: &Key, change: &Change) -> Result<(), String> {
        let (key, change) = (key.clone(), change.clone());
        self.blocking(move |c| {
            c.ensure_tables()?;
            let db = &c.0.database;
            let (exchange, symbol) = key.parts();
            if change.cleared {
                let p = [("s", symbol), ("e", exchange), ("r", key.resolution.as_str()), ("x", key.session())];
                c.query(
                    &format!(
                        "ALTER TABLE {db}.moomoo_bars DELETE WHERE symbol = {{s:String}} AND exchange = {{e:String}}
                         AND resolution = {{r:String}} AND session = {{x:String}} SETTINGS mutations_sync = 1"
                    ),
                    &p,
                    None,
                )?;
            }
            if !change.added.is_empty() {
                let version = c.next_version();
                let rows: String = change
                    .added
                    .iter()
                    .filter_map(|b| {
                        let ts = chrono::DateTime::from_timestamp(b[0] as i64, 0)?.format("%Y-%m-%d %H:%M:%S").to_string();
                        Some(format!(
                            "{}\n",
                            json!({"symbol": symbol, "exchange": exchange, "resolution": key.resolution, "session": key.session(),
                                   "timestamp": ts, "open": b[1], "high": b[2], "low": b[3], "close": b[4], "volume": b[5], "version": version})
                        ))
                    })
                    .collect();
                c.query(&format!("INSERT INTO {db}.moomoo_bars FORMAT JSONEachRow"), &[], Some(rows))?;
            }
            let row = json!({"series": key.id(), "covered": Value::from(serde_json::to_string(&change.covered).unwrap_or_default()), "version": c.next_version()});
            c.query(&format!("INSERT INTO {db}.moomoo_covered FORMAT JSONEachRow"), &[], Some(format!("{row}\n")))?;
            Ok(())
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::bar;
    use super::super::{Backend, Store};
    use super::*;

    #[test]
    fn refuses_what_is_not_an_address_or_a_name() {
        assert!(ClickHouse::new("localhost:8123", "trading_db", "", "").is_err());
        assert!(ClickHouse::new("http://localhost:8123", "x; DROP", "", "").is_err());
        assert!(ClickHouse::new("http://localhost:8123/", "trading_db", "", "").is_ok());
        assert_eq!(encode("a b&c={x:String}"), "a%20b%26c%3D%7Bx%3AString%7D");
    }

    /// Against a real ClickHouse: `OPENTRADER_TEST_CLICKHOUSE=http://localhost:8123 cargo test clickhouse`.
    #[tokio::test]
    async fn a_series_round_trips_through_clickhouse() {
        let Ok(url) = std::env::var("OPENTRADER_TEST_CLICKHOUSE") else {
            eprintln!("skipped: OPENTRADER_TEST_CLICKHOUSE is not set");
            return;
        };
        let db = format!("ot_moomoo_test_{}", std::process::id());
        let ch = ClickHouse::new(&url, &db, "", "").unwrap();
        let key = Key::new("US:AAPL", "5", true);
        let store = Store::new(Backend::ClickHouse(ch.clone()));
        store.update(&key, |s| {
            s.merge(&[bar(300, 1.0), bar(600, 2.0)]);
            s.cover(0, 899);
        }).await;
        store.flush().await;
        store.update(&key, |s| s.merge(&[bar(600, 2.5)])).await;
        store.flush().await;
        let s = ch.load(&key).await.unwrap();
        assert_eq!(s.window(0, 1_000).iter().map(|b| (b[0], b[4])).collect::<Vec<_>>(), [(300.0, 1.0), (600.0, 2.5)]);
        assert_eq!(s.missing(0, 899), []);
        store.update(&key, |s| {
            s.clear();
            s.merge(&[bar(900, 3.0)]);
        }).await;
        store.flush().await;
        let s = ch.load(&key).await.unwrap();
        assert_eq!(s.window(0, 1_000).iter().map(|b| b[4]).collect::<Vec<_>>(), [3.0]);
        let other = ClickHouse::new(&url, "default", "", "").unwrap();
        other.blocking(move |c| c.query(&format!("DROP DATABASE IF EXISTS {db}"), &[], None).map(|_| ())).await.unwrap();
    }
}
