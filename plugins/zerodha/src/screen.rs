/*
 * The screener's India market (specs/screener-plugin-markets.md REQ-SCREEN-MKT-003): the cash-market stocks of NSE
 * and BSE, quoted by Kite's GET /quote (at most 500 instruments a request, 1 request a second), named
 * `EXCHANGE:TRADINGSYMBOL` as the app's symbols of stocks are.
 */
use chrono::{NaiveDateTime, TimeZone};
use chrono_tz::Asia::Kolkata;
use opentrader_plugin_kit::catalog::Instrument;
use opentrader_plugin_kit::screen::Quote;
use serde_json::Value;
use std::collections::HashMap;

/// The market id.
pub const INDIA: &str = "IN";
/// A company on both exchanges is primary on NSE.
pub const PRIMARY: &[&str] = &["NSE", "BSE"];
/// Kite's most instruments in one quote request.
pub const BATCH: usize = 500;

/// A cash-market stock of NSE or BSE (no index, future or option).
pub fn member(market: &str, i: &Instrument) -> bool {
    market == INDIA && i.kind == "stock" && i.instrument == "EQ" && PRIMARY.contains(&i.exchange()) && i.segment == i.exchange()
}

/// `/quote?i=NSE:RELIANCE&i=...`: the instruments by their symbol (`&`, `%`, `+`, `#` and spaces escaped).
pub fn quote_path(insts: &[Instrument]) -> String {
    let enc = |s: &str| s.replace('%', "%25").replace('&', "%26").replace('+', "%2B").replace('#', "%23").replace(' ', "%20");
    let ids: Vec<String> = insts.iter().map(|i| format!("i={}", enc(&i.symbol))).collect();
    format!("/quote?{}", ids.join("&"))
}

/// Kite's `YYYY-MM-DD HH:MM:SS` (India time) as UNIX seconds.
fn ist(v: &Value) -> Option<i64> {
    let t = NaiveDateTime::parse_from_str(v.as_str()?, "%Y-%m-%d %H:%M:%S").ok()?;
    Kolkata.from_local_datetime(&t).earliest().map(|d| d.timestamp())
}

/// The quotes of a `/quote` answer, by symbol. `now` stands for a quote without a timestamp.
pub fn quotes(v: &Value, now: i64) -> Result<HashMap<String, Quote>, String> {
    let data = v.get("data").and_then(Value::as_object).ok_or("Kite quote: no data")?;
    let num = |q: &Value, k: &str| q.get(k).and_then(Value::as_f64).unwrap_or(0.0);
    Ok(data
        .iter()
        .filter_map(|(symbol, q)| {
            let price = q.get("last_price")?.as_f64()?;
            let ohlc = q.get("ohlc").cloned().unwrap_or(Value::Null);
            let quote = Quote {
                time: q.get("timestamp").and_then(ist).unwrap_or(now),
                traded: q.get("last_trade_time").and_then(ist),
                price,
                open: num(&ohlc, "open"),
                high: num(&ohlc, "high"),
                low: num(&ohlc, "low"),
                volume: num(q, "volume"),
                prev_close: Some(num(&ohlc, "close")).filter(|c| *c > 0.0),
            };
            Some((symbol.to_ascii_uppercase(), quote))
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentrader_plugin_kit::catalog::Instrument;
    use serde_json::json;

    fn inst(symbol: &str, kind: &str, instrument: &str, segment: &str) -> Instrument {
        Instrument { symbol: symbol.into(), kind: kind.into(), instrument: instrument.into(), segment: segment.into(), ..Default::default() }
    }

    /// REQ-SCREEN-MKT-003.A1: NSE and BSE stocks are in, indices and derivatives out.
    #[test]
    fn the_market_is_the_cash_stocks_of_nse_and_bse() {
        assert!(member(INDIA, &inst("NSE:RELIANCE", "stock", "EQ", "NSE")));
        assert!(member(INDIA, &inst("BSE:RELIANCE", "stock", "EQ", "BSE")));
        assert!(!member(INDIA, &inst("NSE:NIFTY_50", "index", "EQ", "INDICES")));
        assert!(!member(INDIA, &inst("NFO:NIFTY25NOVFUT", "futures", "FUT", "NFO-FUT")));
        assert!(!member(INDIA, &inst("MCX:GOLD", "stock", "EQ", "MCX")));
        assert!(!member("US", &inst("NSE:RELIANCE", "stock", "EQ", "NSE")));
    }

    #[test]
    fn symbols_are_escaped_in_the_path() {
        let p = quote_path(&[inst("NSE:M&M", "stock", "EQ", "NSE"), inst("BSE:RELIANCE", "stock", "EQ", "BSE")]);
        assert_eq!(p, "/quote?i=NSE:M%26M&i=BSE:RELIANCE");
    }

    #[test]
    fn an_answer_gives_each_symbol_its_quote() {
        let v = json!({"status":"success","data":{"NSE:INFY":{
            "instrument_token":408065,"timestamp":"2025-11-13 15:29:59","last_trade_time":"2025-11-13 15:29:58",
            "last_price":1412.95,"volume":7360198,"net_change":0,"ohlc":{"open":1396,"high":1421.75,"low":1395.55,"close":1389.65}}}});
        let q = quotes(&v, 0).unwrap();
        let infy = q["NSE:INFY"];
        assert_eq!((infy.price, infy.open, infy.high, infy.low, infy.volume, infy.prev_close), (1412.95, 1396.0, 1421.75, 1395.55, 7360198.0, Some(1389.65)));
        // 15:29:59 India time is 09:59:59 UTC.
        assert_eq!(infy.time, 1_763_027_999);
        assert_eq!(infy.traded, Some(1_763_027_998));
        assert!(quotes(&json!({"status":"error"}), 0).is_err());
    }
}
