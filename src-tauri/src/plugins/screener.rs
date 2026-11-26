/*
 * Stock Screener markets served by data plugins (specs/screener-plugin-markets.md, plugins/PROTOCOL.md, data.screen): India from
 * the Zerodha plugin, USA (moomoo) from the moomoo plugin. A plugin declares its markets in its manifest
 * (`screener`); while a screener panel shows one, the app asks the plugin for its rows (`data.screen`) every
 * `POLL` and holds them as a screener table, so scans run on rows the app already holds (REQ-SCREEN-MKT-002).
 *
 * A plugin's rows carry the values the screener shows (the plugin computes them with the live price): the table
 * joins them "at the close", so each is read as sent. Market cap, P/E and P/B come from the inputs the app's own
 * data uses (`shares`, `f.eps_dil_ttm`, `f.book_ps_fq`). Fields the plugin does not give are unavailable.
 *
 * A market's key is `<plugin>:<market>` (`zerodha:IN`).
 */
use super::Plugins;
use chrono::NaiveDate;
use opentrader_core::data::market_rest::{MarketRow, RefTicker};
use opentrader_core::screener::fields::{self, FieldInfo};
use opentrader_core::screener::scan::{self, ScanRequest, ScanResult};
use opentrader_core::screener::state::{StateFile, STATE_VERSION};
use opentrader_core::screener::table::{Join, Row, Table};
use opentrader_core::sync::LockExt;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Manager};
use tauri_specta::Event;

/// How often a shown market's rows are asked for (the plugin refreshes them on its own pace).
const POLL: Duration = Duration::from_secs(10);
const TIMEOUT: Duration = Duration::from_secs(30);

/// A market the enabled data plugins serve.
#[derive(Clone, Debug, PartialEq, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct PluginMarket {
    /// `zerodha:IN`.
    pub key: String,
    pub plugin: String,
    pub plugin_name: String,
    /// `IN`, `US`.
    pub market: String,
}

/// A market's rows as last received, for the screener's status.
#[derive(Clone, Debug, Default, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct PluginMarketStatus {
    pub key: String,
    pub rows: u32,
    pub currency: String,
    /// When the plugin last built the rows (UNIX ms).
    pub updated_ms: Option<f64>,
    /// What the plugin is doing ("Daily history: 812 of 2,040 stocks").
    pub status: Option<String>,
    /// Why the last refresh failed (the plugin's error, or that it did not answer), and when (UNIX ms).
    pub error: Option<String>,
    pub error_ms: Option<f64>,
}

/// After each answer (or failed ask) of a shown market: the panels showing it scan again.
#[derive(Clone, Debug, Serialize, Deserialize, specta::Type, Event)]
#[serde(rename_all = "camelCase")]
pub struct PluginScreenerUpdate {
    pub key: String,
    pub version: u32,
}

/// `data.screen`'s answer (plugins/PROTOCOL.md, data.screen).
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Answer {
    pub currency: String,
    pub rows: Vec<AnswerRow>,
    pub updated: Option<f64>,
    pub status: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AnswerRow {
    pub ticker: String,
    pub name: String,
    pub exchange: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub time: f64,
    pub live: bool,
    pub price: f64,
    pub change: f64,
    pub change_percent: f64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub volume: f64,
    pub prev_close: Option<f64>,
    pub values: BTreeMap<String, Option<f64>>,
}

struct Shown {
    table: Arc<Table>,
    status: PluginMarketStatus,
    owners: HashSet<String>,
    polling: bool,
}

#[derive(Default)]
pub struct PluginMarkets {
    markets: Mutex<Vec<PluginMarket>>,
    shown: Mutex<HashMap<String, Shown>>,
}

/// The screener table of an answer: every row joined at the close, its values the state's.
pub fn table_of(version: u32, a: &Answer, today: NaiveDate) -> Table {
    let refs: Vec<RefTicker> = a
        .rows
        .iter()
        .map(|r| RefTicker { ticker: r.ticker.clone(), name: r.name.clone(), kind: r.kind.clone(), exchange: r.exchange.clone(), currency: a.currency.clone() })
        .collect();
    let n = a.rows.len();
    let mut num: HashMap<String, Vec<Option<f64>>> = HashMap::new();
    for (i, r) in a.rows.iter().enumerate() {
        for (k, v) in &r.values {
            num.entry(k.clone()).or_insert_with(|| vec![None; n])[i] = v.filter(|v| v.is_finite());
        }
    }
    let state = StateFile { version: STATE_VERSION, asof: today, tickers: a.rows.iter().map(|r| r.ticker.clone()).collect(), num, text: HashMap::new() };
    let rows = a
        .rows
        .iter()
        .enumerate()
        .map(|(i, r)| Row {
            m: MarketRow {
                ticker: r.ticker.clone(),
                last: r.price,
                change: r.change,
                change_percent: r.change_percent,
                open: r.open,
                high: r.high,
                low: r.low,
                volume: r.volume,
                prev_close: r.prev_close,
                live: r.live,
                updated_ms: r.time,
                ext: Default::default(),
            },
            r: Some(i),
            st: Some(i),
            join: Join::AtClose,
            day: None,
        })
        .collect();
    Table { version, updated_ms: a.updated, rows, refs: Arc::new(refs), state: Some(Arc::new(state)), complete: true }
}

/// The field catalog of a plugin market: a state field is available only when the plugin gives it.
pub fn catalog_of(t: &Table) -> Vec<FieldInfo> {
    let held = |k: &str| t.state.as_ref().is_some_and(|s| s.num.contains_key(k));
    fields::catalog(t)
        .into_iter()
        .map(|mut f| {
            if f.source == "state" && !f.id.starts_with("f.") {
                f.available = match f.id.as_str() {
                    "market_cap_basic" => held("shares"),
                    "f.pe" | "f.pb" => f.available,
                    id => held(id),
                };
            }
            f
        })
        .collect()
}

impl PluginMarkets {
    fn table(&self, key: &str) -> Arc<Table> {
        self.shown.locked().get(key).map(|s| s.table.clone()).unwrap_or_else(|| Arc::new(Table::empty()))
    }

    fn served(&self, key: &str) -> Option<PluginMarket> {
        self.markets.locked().iter().find(|m| m.key == key).cloned()
    }
}

fn markets(app: &AppHandle) -> tauri::State<'_, PluginMarkets> {
    app.state::<PluginMarkets>()
}

pub fn init(app: &AppHandle) {
    app.manage(PluginMarkets::default());
}

/// The markets of the enabled data plugins: `(plugin id, plugin name, market ids)`.
pub fn set_markets(app: &AppHandle, served: Vec<(String, String, Vec<String>)>) {
    let Some(pm) = app.try_state::<PluginMarkets>() else { return };
    let list: Vec<PluginMarket> = served
        .into_iter()
        .flat_map(|(plugin, name, ms)| {
            ms.into_iter().map(move |m| PluginMarket { key: format!("{plugin}:{m}"), plugin: plugin.clone(), plugin_name: name.clone(), market: m })
        })
        .collect();
    *pm.markets.locked() = list;
}

fn status_of(pm: &PluginMarkets, key: &str) -> PluginMarketStatus {
    pm.shown.locked().get(key).map(|s| s.status.clone()).unwrap_or_else(|| PluginMarketStatus { key: key.into(), ..Default::default() })
}

/// Asks the market's plugin for its rows while a panel shows the market.
async fn poll(app: AppHandle, market: PluginMarket) {
    let mut version = 0u32;
    loop {
        let hub = app.state::<Plugins>().hub.clone();
        let asked = hub.request(&market.plugin, "data.screen", json!({ "market": market.market }), TIMEOUT).await;
        let now_ms = chrono::Utc::now().timestamp_millis() as f64;
        version = version.wrapping_add(1);
        {
            let pm = markets(&app);
            let mut shown = pm.shown.locked();
            let Some(s) = shown.get_mut(&market.key) else { return };
            if s.owners.is_empty() {
                s.polling = false;
                return;
            }
            match asked.map_err(|e| e.to_string()).and_then(|v| serde_json::from_value::<Answer>(v).map_err(|e| format!("{}: {e}", market.plugin_name))) {
                Ok(a) => {
                    let today = chrono::Utc::now().date_naive();
                    s.table = Arc::new(table_of(version, &a, today));
                    s.status = PluginMarketStatus {
                        key: market.key.clone(),
                        rows: a.rows.len() as u32,
                        currency: a.currency.clone(),
                        updated_ms: a.updated,
                        status: a.status.clone(),
                        error_ms: a.error.as_ref().map(|_| now_ms),
                        error: a.error,
                    };
                }
                // The last rows stay (REQ-SCREEN-MKT-002.A3).
                Err(e) => {
                    s.status.error = Some(e);
                    s.status.error_ms = Some(now_ms);
                }
            }
        }
        let _ = PluginScreenerUpdate { key: market.key.clone(), version }.emit(&app);
        tokio::time::sleep(POLL).await;
    }
}

#[tauri::command]
#[specta::specta]
pub fn plugin_screener_markets(app: AppHandle) -> Vec<PluginMarket> {
    markets(&app).markets.locked().clone()
}

/// Start showing `key` for `owner` ("<window label>:screener").
#[tauri::command]
#[specta::specta]
pub fn plugin_screener_open(app: AppHandle, key: String, owner: String) -> Result<PluginMarketStatus, String> {
    let pm = markets(&app);
    let market = pm.served(&key).ok_or_else(|| format!("No enabled plugin serves the screener market {key}"))?;
    let start = {
        let mut shown = pm.shown.locked();
        let s = shown.entry(key.clone()).or_insert_with(|| Shown {
            table: Arc::new(Table::empty()),
            status: PluginMarketStatus { key: key.clone(), ..Default::default() },
            owners: HashSet::new(),
            polling: false,
        });
        s.owners.insert(owner);
        !std::mem::replace(&mut s.polling, true)
    };
    if start {
        tauri::async_runtime::spawn(poll(app.clone(), market));
    }
    Ok(status_of(&pm, &key))
}

/// Stop showing `key` for `owner`; with no owner left, the plugin is no longer asked (REQ-SCREEN-MKT-002.A1).
#[tauri::command]
#[specta::specta]
pub fn plugin_screener_close(app: AppHandle, key: String, owner: String) {
    if let Some(s) = markets(&app).shown.locked().get_mut(&key) {
        s.owners.remove(&owner);
    }
}

#[tauri::command]
#[specta::specta]
pub fn plugin_screener_scan(app: AppHandle, key: String, req: ScanRequest) -> Result<ScanResult, String> {
    scan::run(&markets(&app).table(&key), &req).map_err(|e| e.to_string())
}

#[tauri::command]
#[specta::specta]
pub fn plugin_screener_fields(app: AppHandle, key: String) -> Vec<FieldInfo> {
    catalog_of(&markets(&app).table(&key))
}

#[tauri::command]
#[specta::specta]
pub fn plugin_screener_values(app: AppHandle, key: String, field: String) -> Result<Vec<String>, String> {
    fields::text_values(&markets(&app).table(&key), &field).map_err(|e| e.to_string())
}

#[tauri::command]
#[specta::specta]
pub fn plugin_screener_status(app: AppHandle, key: String) -> PluginMarketStatus {
    status_of(&markets(&app), &key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentrader_core::screener::scan::ScanRequest;

    fn answer() -> Answer {
        serde_json::from_value(json!({
            "market": "IN", "currency": "INR", "updated": 1763027999000.0, "fields": ["SMA50"],
            "rows": [
                {"ticker": "NSE:RELIANCE", "name": "Reliance", "exchange": "NSE", "type": "stock", "time": 1763027999000.0, "live": true,
                 "price": 1500.0, "change": 15.0, "changePercent": 1.01, "open": 1490.0, "high": 1510.0, "low": 1480.0, "volume": 1000000.0,
                 "prevClose": 1485.0, "values": {"SMA50": 1450.5, "shares": 1000.0}},
                {"ticker": "BSE:RELIANCE", "name": "Reliance", "exchange": "BSE", "type": "secondary", "time": 0, "live": false,
                 "price": 0, "change": 0, "changePercent": 0, "open": 0, "high": 0, "low": 0, "volume": 0, "values": {}}
            ]
        }))
        .unwrap()
    }

    fn scan(t: &Table, req: serde_json::Value) -> ScanResult {
        let req: ScanRequest = serde_json::from_value(req).unwrap();
        scan::run(t, &req).unwrap()
    }

    /// REQ-SCREEN-MKT-002: the plugin's values are read as sent; the primary listing filter keeps one row.
    #[test]
    fn a_plugin_answer_scans_like_the_market() {
        let t = table_of(1, &answer(), NaiveDate::from_ymd_opt(2025, 11, 13).unwrap());
        let r = scan(&t, json!({"columns": ["name", "close", "SMA50", "market_cap_basic", "exchange", "currency"],
            "filter": [{"left": "type", "operation": "in_range", "right": ["stock"]}], "range": [0, 50]}));
        assert_eq!(r.total_count, 1);
        let d = serde_json::to_value(&r.rows[0]).unwrap();
        assert_eq!(d["d"], json!(["NSE:RELIANCE", 1500.0, 1450.5, 1_500_000.0, "NSE", "INR"]), "{d}");
    }

    /// REQ-SCREEN-MKT-002.A2: a field the plugin does not give is unavailable and matches nothing.
    #[test]
    fn a_field_not_given_is_unavailable() {
        let t = table_of(1, &answer(), NaiveDate::from_ymd_opt(2025, 11, 13).unwrap());
        let cat = catalog_of(&t);
        let avail = |id: &str| cat.iter().find(|f| f.id == id).map(|f| f.available);
        assert_eq!((avail("SMA50"), avail("SMA200"), avail("market_cap_basic"), avail("close")), (Some(true), Some(false), Some(true), Some(true)));
        let r = scan(&t, json!({"columns": ["name"], "filter": [{"left": "SMA200", "operation": "greater", "right": 0}], "range": [0, 50]}));
        assert_eq!(r.total_count, 0);
    }
}
