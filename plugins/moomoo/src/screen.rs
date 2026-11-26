/*
 * The screener's "USA (moomoo)" market (specs/screener-plugin-markets.md REQ-SCREEN-MKT-005): the US stocks of the
 * plugin's US list, with OpenD's snapshot (Qot_GetSecuritySnapshot, 3203: at most 400 securities a request, 60
 * requests in 30 s; this asks for at most half of that). Values use the screener's field ids (plugins/PROTOCOL.md, data.screen):
 *   shares          issued shares (the app's market cap is the price times this)
 *   f.eps_dil_ttm   the price over the TTM P/E (the app's P/E is the price over this)
 *   f.book_ps_fq    net assets per share (the app's P/B)
 *   dividends_yield_current, price_52_week_high, price_52_week_low   as the snapshot gives them
 */
use crate::market::Market;
use crate::opend::client::Client;
use crate::opend::proto;
use crate::search::Listed;
use opentrader_plugin_kit::pace::Pace;
use opentrader_plugin_kit::screen::{self, Quote, Row, State};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

pub const US: &str = "US";
/// OpenD's most securities in one snapshot request.
pub const BATCH: usize = 400;
/// Half of OpenD's snapshot limit (60 requests in 30 s).
pub fn pace() -> Pace {
    Pace::new(30, Duration::from_secs(30))
}
/// How often the rows are refreshed while a US session (pre-market to post-market) is open, and otherwise.
const OPEN_EVERY: Duration = Duration::from_secs(30);
const CLOSED_EVERY: Duration = Duration::from_secs(300);

pub const FIELDS: &[&str] = &["shares", "f.eps_dil_ttm", "f.book_ps_fq", "dividends_yield_current", "price_52_week_high", "price_52_week_low"];

/// A JSON number, or a 64-bit integer written as a string.
fn num(v: Option<&Value>) -> Option<f64> {
    let v = v?;
    v.as_f64().or_else(|| v.as_str()?.parse().ok()).filter(|x: &f64| x.is_finite())
}

/// One snapshot: its quote and values.
pub fn snapshot(s: &Value, now: i64) -> Option<(String, Quote, BTreeMap<String, f64>)> {
    let b = s.get("basic")?;
    let sec = b.get("security")?;
    let code = sec.get("code")?.as_str()?;
    let symbol = format!("US:{}", code.to_ascii_uppercase());
    let n = |k: &str| num(b.get(k));
    let price = n("curPrice")?;
    let time = n("updateTimestamp").map_or(now, |t| t as i64);
    let q = Quote {
        time,
        traded: Some(time),
        price,
        open: n("openPrice").unwrap_or(0.0),
        high: n("highPrice").unwrap_or(0.0),
        low: n("lowPrice").unwrap_or(0.0),
        volume: n("volume").unwrap_or(0.0),
        prev_close: n("lastClosePrice").filter(|p| *p > 0.0),
    };
    let mut v = BTreeMap::new();
    let mut put = |k: &str, x: Option<f64>| {
        if let Some(x) = x.filter(|x| x.is_finite()) {
            v.insert(k.to_string(), x);
        }
    };
    put("price_52_week_high", n("highest52WeeksPrice").filter(|x| *x > 0.0));
    put("price_52_week_low", n("lowest52WeeksPrice").filter(|x| *x > 0.0));
    if let Some(e) = s.get("equityExData") {
        let e = |k: &str| num(e.get(k));
        put("shares", e("issuedShares").filter(|x| *x > 0.0));
        put("f.eps_dil_ttm", e("peTTMRate").filter(|pe| *pe != 0.0).map(|pe| price / pe));
        put("f.book_ps_fq", e("netAssetPershare"));
        // A percent (20 is 20%).
        put("dividends_yield_current", e("dividendRatioTTM"));
    }
    Some((symbol, q, v))
}

/// The rows of `stocks` from the snapshots taken.
pub fn rows(stocks: &[Listed], snaps: &HashMap<String, (Quote, BTreeMap<String, f64>)>, now: i64) -> Vec<Row> {
    let today = crate::hours::next_midnight(Market::Us, now) - 86_400;
    stocks
        .iter()
        .map(|l| {
            let s = snaps.get(&l.symbol);
            let mut r = screen::row(&l.symbol, &l.name, "stock", s.map(|s| &s.0), today, s.map(|s| s.1.clone()).unwrap_or_default());
            r.exchange = "US".into();
            r
        })
        .collect()
}

/// Keeps the rows current while the app asks for them.
pub async fn run(state: &State, client: &Client, stocks: impl Fn() -> Vec<Listed>, pace: &Pace) {
    let mut snaps: HashMap<String, (Quote, BTreeMap<String, f64>)> = HashMap::new();
    while state.wanted() {
        let started = Instant::now();
        let list = stocks();
        let mut error = None;
        for chunk in list.chunks(BATCH) {
            if !state.wanted() {
                return;
            }
            pace.wait().await;
            let secs: Vec<Value> = chunk.iter().map(|l| json!({ "market": Market::Us.qot(), "code": l.code })).collect();
            match client.request(proto::QOT_SECURITY_SNAPSHOT, json!({ "securityList": secs })).await {
                Ok(s2c) => {
                    let t = opentrader_plugin_kit::now();
                    for s in s2c.get("snapshotList").and_then(Value::as_array).into_iter().flatten() {
                        if let Some((sym, q, v)) = snapshot(s, t) {
                            snaps.insert(sym, (q, v));
                        }
                    }
                }
                Err(e) => {
                    error = Some(if e.contains("not reachable") { format!("OpenD is not connected ({e})") } else { e });
                    break;
                }
            }
        }
        let t = opentrader_plugin_kit::now();
        let rows = rows(&list, &snaps, t);
        state.update(|a| {
            a.market = US.into();
            a.currency = "USD".into();
            a.rows = rows;
            a.fields = FIELDS.iter().map(|f| f.to_string()).collect();
            a.updated = Some(t as f64 * 1000.0);
            a.status = list.is_empty().then(|| "Loading moomoo's US list".to_string());
            a.error = error;
        });
        let every = if crate::hours::is_open(Market::Us, true, t) { OPEN_EVERY } else { CLOSED_EVERY };
        let until = started + every;
        while Instant::now() < until {
            if !state.wanted() {
                return;
            }
            tokio::time::sleep(Duration::from_secs(1).min(until.saturating_duration_since(Instant::now()))).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// REQ-SCREEN-MKT-005.A1: a snapshot gives the quote and the inputs of market cap, P/E and P/B.
    #[test]
    fn a_snapshot_gives_quote_and_fundamentals() {
        let s = json!({"basic":{"security":{"market":11,"code":"AAPL"},"curPrice":200.0,"openPrice":198.0,"highPrice":201.0,
            "lowPrice":197.5,"lastClosePrice":196.0,"volume":"51234567","updateTimestamp":1763060000.5,
            "highest52WeeksPrice":260.1,"lowest52WeeksPrice":169.2},
            "equityExData":{"issuedShares":"14840390000","peTTMRate":32.0,"netAssetPershare":4.4,"dividendRatioTTM":0.5}});
        let (sym, q, v) = snapshot(&s, 0).unwrap();
        assert_eq!(sym, "US:AAPL");
        assert_eq!((q.price, q.volume, q.prev_close, q.time), (200.0, 51_234_567.0, Some(196.0), 1_763_060_000));
        assert_eq!(v["shares"], 14_840_390_000.0);
        assert_eq!(v["f.eps_dil_ttm"], 6.25);
        assert_eq!((v["f.book_ps_fq"], v["dividends_yield_current"], v["price_52_week_high"]), (4.4, 0.5, 260.1));
    }

    #[test]
    fn a_stock_without_a_snapshot_has_an_empty_row() {
        let l = Listed { symbol: "US:XYZ".into(), code: "XYZ".into(), name: "Xyz".into(), kind: "stock" };
        let r = rows(&[l], &HashMap::new(), 1_763_060_000);
        assert_eq!((r[0].price, r[0].exchange.as_str(), r[0].values.len()), (0.0, "US", 0));
    }
}
