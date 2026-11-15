/*
 * `data.search` over each market's list of stocks, funds and indexes, loaded from OpenD once (Qot_GetStaticInfo by
 * market and security type) and kept for the session: exact code first, then code prefix, then name match.
 */
use crate::market::{symbol_of, Market};
use crate::opend::client::Client;
use crate::opend::proto::{self, StaticBasic};
use serde_json::{json, Value};
use std::collections::HashMap;
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

/// The lists, loaded on first use.
#[derive(Default)]
pub struct Catalog {
    lists: Mutex<HashMap<Market, Vec<Listed>>>,
}

impl Catalog {
    async fn load(client: &Client, market: Market) -> Result<Vec<Listed>, String> {
        let mut out = Vec::new();
        for (sec_type, _) in TYPES {
            let s2c = client.request(proto::QOT_STATIC_INFO, json!({ "market": market.qot(), "secType": sec_type })).await?;
            out.extend(listed(&s2c));
        }
        Ok(out)
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
