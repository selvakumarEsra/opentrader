/*
 * `data.search` over each market's list of stocks, funds and indexes, loaded from OpenD once (Qot_GetStaticInfo by
 * market and security type) and kept for the session: exact code first, then code prefix, then name match. The
 * US list is also kept on disk (`symbols-US.json`, next to the bar store): loaded on the first run, read at start
 * after that, and loaded again when a day old.
 */
use crate::market::{symbol_of, Market};
use crate::opend::client::Client;
use crate::opend::proto::{self, StaticBasic};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::sync::Mutex;

pub const MAX_ROWS: usize = 50;
/// OpenD's SecurityType values listed, and the app's search type of each.
const TYPES: [(i32, &str); 3] = [(3, "stock"), (4, "fund"), (6, "index")];

#[derive(Debug, Clone, PartialEq)]
pub struct Listed {
    pub symbol: String,
    pub code: String,
    pub name: String,
    pub kind: &'static str,
}

pub fn kind_of(sec_type: i32) -> Option<&'static str> {
    TYPES.iter().find(|(t, _)| *t == sec_type).map(|(_, k)| *k)
}

/// The best `MAX_ROWS` of `list` for `query`.
pub fn rank<'a>(query: &str, list: impl IntoIterator<Item = &'a Listed>) -> Vec<&'a Listed> {
    let q = query.trim().to_ascii_uppercase();
    if q.is_empty() {
        return Vec::new();
    }
    let mut hits: Vec<(u8, usize, &Listed)> = list
        .into_iter()
        .filter_map(|l| {
            let score = if l.code == q {
                0
            } else if l.code.starts_with(&q) {
                1
            } else if l.name.to_ascii_uppercase().contains(&q) {
                2
            } else {
                return None;
            };
            Some((score, l.code.len(), l))
        })
        .collect();
    hits.sort_by(|a, b| (a.0, a.1, &a.2.code).cmp(&(b.0, b.1, &b.2.code)));
    hits.into_iter().take(MAX_ROWS).map(|(_, _, l)| l).collect()
}

/// How long a US list read from disk is used before it is loaded again from OpenD.
pub const LIST_FRESH_SECS: i64 = 24 * 3600;
/// The file of the US list, next to the bar store.
pub const US_LIST_FILE: &str = "symbols-US.json";

/// The lists: the US one read from its file at start and kept fresh (`keep_fresh`), the others loaded on first use.
#[derive(Default)]
pub struct Catalog {
    lists: Mutex<HashMap<Market, Vec<Listed>>>,
    /// The folder of the US list file; none keeps it in memory only.
    dir: Option<PathBuf>,
    /// When the US list in use was loaded from OpenD (UNIX seconds).
    us_fetched: Mutex<Option<i64>>,
    refreshing: Mutex<()>,
}

#[derive(Serialize, Deserialize)]
struct ListFile {
    fetched: i64,
    list: Vec<FileRow>,
}

#[derive(Serialize, Deserialize)]
struct FileRow {
    symbol: String,
    code: String,
    name: String,
    kind: String,
}

impl Catalog {
    /// The catalog with the US list of `dir`'s file, when there is a readable one.
    pub fn with_dir(dir: Option<PathBuf>) -> Catalog {
        let file = dir.as_ref().and_then(|d| std::fs::read(d.join(US_LIST_FILE)).ok());
        let read = file.and_then(|b| serde_json::from_slice::<ListFile>(&b).ok());
        let mut lists = HashMap::new();
        let mut fetched = None;
        if let Some(f) = read {
            let list: Vec<Listed> = f
                .list
                .into_iter()
                .filter_map(|r| {
                    let kind = TYPES.iter().find(|(_, k)| *k == r.kind).map(|(_, k)| *k)?;
                    Some(Listed { symbol: r.symbol, code: r.code, name: r.name, kind })
                })
                .collect();
            if !list.is_empty() {
                lists.insert(Market::Us, list);
                fetched = Some(f.fetched);
            }
        }
        Catalog { lists: Mutex::new(lists), dir, us_fetched: Mutex::new(fetched), refreshing: Mutex::new(()) }
    }

    async fn load(client: &Client, market: Market) -> Result<Vec<Listed>, String> {
        let mut out = Vec::new();
        for (sec_type, _) in TYPES {
            let s2c = client.request(proto::QOT_STATIC_INFO, json!({ "market": market.qot(), "secType": sec_type })).await?;
            out.extend(listed(&s2c));
        }
        Ok(out)
    }

    /// Loads the whole US list from OpenD when there is none or it is older than a day, and writes its file. Ok(true)
    /// when it was loaded. Searches go on with the list in use meanwhile; a failed load keeps it.
    pub async fn keep_fresh(&self, client: &Client, now: i64) -> Result<bool, String> {
        // One load at a time: a reconnect during a load finds the list fresh.
        let _one = self.refreshing.lock().await;
        if self.us_fetched.lock().await.is_some_and(|t| now - t < LIST_FRESH_SECS) {
            return Ok(false);
        }
        let list = Self::load(client, Market::Us).await?;
        if list.is_empty() {
            return Err("OpenD listed no US securities".into());
        }
        if let Some(dir) = &self.dir {
            let file = ListFile {
                fetched: now,
                list: list.iter().map(|l| FileRow { symbol: l.symbol.clone(), code: l.code.clone(), name: l.name.clone(), kind: l.kind.into() }).collect(),
            };
            let written = std::fs::create_dir_all(dir).and_then(|_| {
                let tmp = dir.join(format!("{US_LIST_FILE}.tmp"));
                std::fs::write(&tmp, serde_json::to_vec(&file).unwrap_or_default())?;
                std::fs::rename(&tmp, dir.join(US_LIST_FILE))
            });
            if let Err(e) = written {
                println!("[warn] the US list could not be written to {}: {e}", dir.display());
            }
        }
        println!("[info] loaded the US list: {} stocks, funds and indexes", list.len());
        self.lists.lock().await.insert(Market::Us, list);
        *self.us_fetched.lock().await = Some(now);
        Ok(true)
    }

    pub async fn search(&self, client: &Client, query: &str) -> Vec<Value> {
        let mut lists = self.lists.lock().await;
        for m in Market::ALL {
            if let std::collections::hash_map::Entry::Vacant(slot) = lists.entry(m) {
                match Self::load(client, m).await {
                    Ok(l) => {
                        slot.insert(l);
                    }
                    // Not kept: the next search asks again (OpenD down, no rights to that market yet).
                    Err(e) => println!("[warn] the {} list could not be loaded: {e}", m.exchange()),
                }
            }
        }
        rank(query, lists.values().flatten())
            .into_iter()
            .map(|l| json!({ "ticker": l.symbol, "description": l.name, "type": l.kind }))
            .collect()
    }

    /// The name and type of `symbol`, when its market's list is loaded.
    pub async fn find(&self, symbol: &str) -> Option<Listed> {
        self.lists.lock().await.values().flatten().find(|l| l.symbol == symbol).cloned()
    }
}

/// The rows of a Qot_GetStaticInfo answer: listed securities of a served market and type.
pub fn listed(s2c: &Value) -> Vec<Listed> {
    let infos = s2c.get("staticInfoList").and_then(Value::as_array).cloned().unwrap_or_default();
    infos
        .iter()
        .filter_map(|i| {
            let b: StaticBasic = serde_json::from_value(i.get("basic")?.clone()).ok()?;
            let kind = kind_of(b.sec_type)?;
            let symbol = symbol_of(&b.security).filter(|_| !b.delisting)?;
            Some(Listed { symbol, code: b.security.code, name: b.name, kind })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn l(code: &str, name: &str) -> Listed {
        Listed { symbol: format!("US:{code}"), code: code.into(), name: name.into(), kind: "stock" }
    }

    #[test]
    fn exact_code_then_prefix_then_name() {
        let list = [l("AAPU", "Direxion Daily AAPL Bull"), l("XYZ", "Aapex Holdings"), l("AAPL", "Apple Inc"), l("MSFT", "Microsoft")];
        let found: Vec<&str> = rank("aap", &list).iter().map(|x| x.code.as_str()).collect();
        assert_eq!(found, ["AAPL", "AAPU", "XYZ"]);
        let found: Vec<&str> = rank("AAPL", &list).iter().map(|x| x.code.as_str()).collect();
        assert_eq!(found, ["AAPL", "AAPU"]);
        assert!(rank("  ", &list).is_empty());
    }

    #[test]
    fn at_most_fifty_rows() {
        let list: Vec<Listed> = (0..80).map(|i| l(&format!("A{i}"), "")).collect();
        assert_eq!(rank("A", &list).len(), MAX_ROWS);
    }

    #[test]
    fn reads_listed_securities_of_served_markets_and_types() {
        let s2c = json!({"staticInfoList":[
            {"basic":{"security":{"market":11,"code":"AAPL"},"id":"1","lotSize":1,"secType":3,"name":"Apple","listTime":""}},
            {"basic":{"security":{"market":11,"code":"SPY"},"id":"2","lotSize":1,"secType":4,"name":"SPDR S&P 500","listTime":""}},
            {"basic":{"security":{"market":11,"code":"W"},"id":"3","lotSize":1,"secType":5,"name":"a warrant","listTime":""}},
            {"basic":{"security":{"market":11,"code":"OLD"},"id":"4","lotSize":1,"secType":3,"name":"gone","listTime":"","delisting":true}},
            {"basic":{"security":{"market":2,"code":"F"},"id":"5","lotSize":1,"secType":3,"name":"elsewhere","listTime":""}}
        ]});
        let rows = listed(&s2c);
        let got: Vec<(&str, &str)> = rows.iter().map(|r| (r.symbol.as_str(), r.kind)).collect();
        assert_eq!(got, [("US:AAPL", "stock"), ("US:SPY", "fund")]);
    }
}
