/*
 * Kite's history: GET /instruments/historical/<token>/<interval>?from=&to= (IST wall times, both inclusive), each
 * candle `[time, open, high, low, close, volume]` with time like `2024-11-14T09:15:00+0530` (the bar's start). One
 * request may span at most: minute 60 days, 3-10 minute 100, 15-30 minute 200, 60 minute 400, day 2000.
 */
use opentrader_plugin_kit::Bar;
use serde_json::Value;

pub const INTERVALS: &[&str] = &["1", "3", "5", "10", "15", "30", "60", "1D"];

/// Kite's interval name and the longest span (days) of one request.
pub fn interval_of(resolution: &str) -> Option<(&'static str, i64)> {
    Some(match resolution {
        "1" => ("minute", 60),
        "3" => ("3minute", 100),
        "5" => ("5minute", 100),
        "10" => ("10minute", 100),
        "15" => ("15minute", 200),
        "30" => ("30minute", 200),
        "60" => ("60minute", 400),
        "1D" | "D" => ("day", 2000),
        _ => return None,
    })
}

/// A UNIX time as Kite's `from`/`to`: `yyyy-mm-dd hh:mm:ss` in IST.
pub fn ist_text(unix: i64) -> String {
    chrono::DateTime::from_timestamp(unix + 19_800, 0).map(|d| d.format("%Y-%m-%d %H:%M:%S").to_string()).unwrap_or_default()
}

/// The bars of a historical answer.
pub fn candles(v: &Value) -> Result<Vec<Bar>, String> {
    if v["status"] != "success" {
        return Err(v["message"].as_str().unwrap_or("Kite refused the history").to_string());
    }
    let rows = v["data"]["candles"].as_array().cloned().unwrap_or_default();
    let mut out: Vec<Bar> = rows
        .iter()
        .filter_map(|c| {
            let t = chrono::DateTime::parse_from_str(c.get(0)?.as_str()?, "%Y-%m-%dT%H:%M:%S%z").ok()?.timestamp();
            let n = |i: usize| c.get(i).and_then(Value::as_f64);
            Some([t as f64, n(1)?, n(2)?, n(3)?, n(4)?, n(5).unwrap_or(0.0)])
        })
        .collect();
    out.sort_by(|a, b| a[0].total_cmp(&b[0]));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn candles_are_stamped_at_their_start() {
        let v = json!({"status":"success","data":{"candles":[
            ["2024-11-14T09:16:00+0530", 2, 3, 1, 2.5, 20],
            ["2024-11-14T09:15:00+0530", 1704.5, 1705, 1699.25, 1702.8, 2499],
            ["2024-11-14T00:00:00+0530", 1, 1, 1, 1, 1]
        ]}});
        let bars = candles(&v).unwrap();
        assert_eq!(bars[1], [1_731_555_900.0, 1704.5, 1705.0, 1699.25, 1702.8, 2499.0]);
        // A daily candle starts at midnight IST.
        assert_eq!(bars[0][0] as i64, 1_731_522_600);
        assert_eq!(bars[2][0] as i64, 1_731_555_960);
    }

    #[test]
    fn refusals_and_wall_times() {
        assert_eq!(candles(&json!({"status":"error","message":"Insufficient permission"})).unwrap_err(), "Insufficient permission");
        assert_eq!(ist_text(1_731_555_900), "2024-11-14 09:15:00");
        assert_eq!(interval_of("1"), Some(("minute", 60)));
        assert_eq!(interval_of("2"), None);
    }
}
