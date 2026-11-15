/*
 * Dhan's history: POST /v2/charts/historical (daily, from a scrip's start; `toDate` not included) and
 * POST /v2/charts/intraday (1, 5, 15, 25 or 60 minutes, at most 90 days a request, the last five years), each
 * answering parallel arrays `open`, `high`, `low`, `close`, `volume`, `timestamp` (UNIX seconds, the bar's start).
 */
use opentrader_plugin_kit::calendar::Calendar;
use opentrader_plugin_kit::catalog::Instrument;
use opentrader_plugin_kit::Bar;
use serde_json::{json, Value};

pub const INTERVALS: &[&str] = &["1", "5", "15", "60", "1D"];
/// How far back intraday bars go.
pub const INTRADAY_YEARS: i64 = 5;

/// The longest span (days) of one request at `resolution`.
pub fn step_days(resolution: &str) -> Option<i64> {
    match resolution {
        "1" | "5" | "15" | "60" => Some(90),
        "1D" | "D" => Some(3650),
        _ => None,
    }
}

fn ist(unix: i64, format: &str) -> String {
    chrono::DateTime::from_timestamp(unix + 19_800, 0).map(|d| d.format(format).to_string()).unwrap_or_default()
}

/// The request of `inst`'s bars starting in [from, to]: its path and body.
pub fn request(inst: &Instrument, resolution: &str, from: i64, to: i64) -> (&'static str, Value) {
    if matches!(resolution, "1D" | "D") {
        let body = json!({
            "securityId": inst.id, "exchangeSegment": inst.segment, "instrument": inst.instrument, "expiryCode": 0, "oi": false,
            "fromDate": ist(from, "%Y-%m-%d"), "toDate": ist(to + 86_400, "%Y-%m-%d"),
        });
        ("/v2/charts/historical", body)
    } else {
        let body = json!({
            "securityId": inst.id, "exchangeSegment": inst.segment, "instrument": inst.instrument, "interval": resolution, "oi": false,
            "fromDate": ist(from, "%Y-%m-%d %H:%M:%S"), "toDate": ist(to + 60, "%Y-%m-%d %H:%M:%S"),
        });
        ("/v2/charts/intraday", body)
    }
}

/// The bars of an answer; daily bars start at their day's midnight in Asia/Kolkata.
pub fn candles(v: &Value, daily: bool) -> Result<Vec<Bar>, String> {
    if let Some(why) = v["errorMessage"].as_str() {
        return Err(format!("Dhan: {why}"));
    }
    let col = |k: &str| v[k].as_array().cloned().unwrap_or_default();
    let (o, h, l, c, vol, t) = (col("open"), col("high"), col("low"), col("close"), col("volume"), col("timestamp"));
    let cal = Calendar::india("NSE");
    let mut out: Vec<Bar> = (0..t.len().min(o.len()).min(h.len()).min(l.len()).min(c.len()))
        .filter_map(|i| {
            let time = t[i].as_f64()? as i64;
            let start = if daily { cal.midnight(time) } else { time };
            Some([start as f64, o[i].as_f64()?, h[i].as_f64()?, l[i].as_f64()?, c[i].as_f64()?, vol.get(i).and_then(Value::as_f64).unwrap_or(0.0)])
        })
        .collect();
    out.sort_by(|a, b| a[0].total_cmp(&b[0]));
    out.dedup_by(|a, b| a[0] == b[0]);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reliance() -> Instrument {
        Instrument { symbol: "NSE:RELIANCE".into(), id: "2885".into(), segment: "NSE_EQ".into(), instrument: "EQUITY".into(), ..Default::default() }
    }

    #[test]
    fn requests_name_the_scrip_and_ist_times() {
        // 2025-11-13 09:15 to 09:20 IST.
        let (path, body) = request(&reliance(), "1", 1_763_005_500, 1_763_005_800);
        assert_eq!(path, "/v2/charts/intraday");
        assert_eq!(body, json!({"securityId":"2885","exchangeSegment":"NSE_EQ","instrument":"EQUITY","interval":"1","oi":false,
            "fromDate":"2025-11-13 09:15:00","toDate":"2025-11-13 09:21:00"}));
        let (path, body) = request(&reliance(), "1D", 1_762_972_200, 1_762_972_200);
        assert_eq!((path, body["fromDate"].as_str(), body["toDate"].as_str()), ("/v2/charts/historical", Some("2025-11-13"), Some("2025-11-14")));
    }

    #[test]
    fn arrays_become_bars() {
        let v = json!({"open":[1.0,2.0],"high":[1.5,2.5],"low":[0.5,1.5],"close":[1.2,2.2],"volume":[10,20],"timestamp":[1_763_005_560,1_763_005_500]});
        assert_eq!(candles(&v, false).unwrap(), vec![[1_763_005_500.0, 2.0, 2.5, 1.5, 2.2, 20.0], [1_763_005_560.0, 1.0, 1.5, 0.5, 1.2, 10.0]]);
        // A daily bar stamped in the day starts at its midnight.
        let d = json!({"open":[1],"high":[1],"low":[1],"close":[1],"volume":[1],"timestamp":[1_763_005_500]});
        assert_eq!(candles(&d, true).unwrap()[0][0] as i64, 1_762_972_200);
        assert_eq!(candles(&json!({"errorType":"Data_Error","errorCode":"DH-905","errorMessage":"No data"}), false).unwrap_err(), "Dhan: No data");
    }
}
