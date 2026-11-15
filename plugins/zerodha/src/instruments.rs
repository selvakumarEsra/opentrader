/*
 * Kite's instrument dump (GET /instruments, a CSV, gzipped): instrument_token, exchange_token, tradingsymbol, name,
 * last_price, expiry, strike, tick_size, lot_size, instrument_type, segment, exchange. Each row of a served exchange
 * becomes an `Instrument` named `<exchange>:<tradingsymbol>`: indices (segment INDICES) are `index`, cash (EQ)
 * `stock`, futures and options `futures`. The token is its id and its stream key.
 */
use opentrader_plugin_kit::catalog::{ticker_of, Instrument};
use std::io::Read;

pub const EXCHANGES: [&str; 8] = ["NSE", "BSE", "NFO", "BFO", "MCX", "CDS", "BCD", "NCO"];

/// The text of a dump body, gunzipped when it is gzip.
pub fn text_of(body: Vec<u8>) -> Result<String, String> {
    if body.starts_with(&[0x1f, 0x8b]) {
        let mut out = String::new();
        flate2::read::GzDecoder::new(&body[..]).read_to_string(&mut out).map_err(|e| format!("the instrument list: {e}"))?;
        Ok(out)
    } else {
        String::from_utf8(body).map_err(|e| format!("the instrument list: {e}"))
    }
}

/// `2026-11-24` → `24/11/2026`.
pub fn shown_date(iso: &str) -> String {
    let p: Vec<&str> = iso.split('-').collect();
    if p.len() == 3 {
        format!("{}/{}/{}", p[2], p[1], p[0])
    } else {
        iso.to_string()
    }
}

/// A strike as text without a needless `.0`.
pub fn strike_text(s: f64) -> String {
    if s.fract() == 0.0 {
        format!("{}", s as i64)
    } else {
        format!("{s}")
    }
}

pub fn parse(csv_text: &str) -> Result<Vec<Instrument>, String> {
    let mut reader = csv::ReaderBuilder::new().flexible(true).from_reader(csv_text.as_bytes());
    let headers = reader.headers().map_err(|e| format!("the instrument list: {e}"))?.clone();
    let col = |name: &str| headers.iter().position(|h| h.trim() == name);
    let (Some(token), Some(symbol), Some(name), Some(expiry), Some(strike), Some(tick), Some(lot), Some(kind), Some(segment), Some(exchange)) = (
        col("instrument_token"),
        col("tradingsymbol"),
        col("name"),
        col("expiry"),
        col("strike"),
        col("tick_size"),
        col("lot_size"),
        col("instrument_type"),
        col("segment"),
        col("exchange"),
    ) else {
        return Err("the instrument list has not the expected columns".into());
    };
    let mut out = Vec::new();
    for row in reader.records().flatten() {
        let get = |i: usize| row.get(i).unwrap_or("").trim();
        let exchange = get(exchange);
        if !EXCHANGES.contains(&exchange) {
            continue;
        }
        let (segment, itype) = (get(segment), get(kind));
        let class = if segment == "INDICES" {
            "index"
        } else {
            match itype {
                "EQ" => "stock",
                "FUT" | "CE" | "PE" => "futures",
                _ => continue,
            }
        };
        let trading = get(symbol);
        if trading.is_empty() {
            continue;
        }
        let expiry = Some(get(expiry).to_string()).filter(|e| !e.is_empty());
        let strike = get(strike).parse::<f64>().ok().filter(|s| *s > 0.0);
        let option = matches!(itype, "CE" | "PE").then(|| itype.to_string());
        let underlying = get(name);
        let shown = match (class, &expiry) {
            ("futures", Some(e)) => match (&option, strike) {
                (Some(o), Some(s)) => format!("{underlying} {} {} {o}", shown_date(e), strike_text(s)),
                _ => format!("{underlying} {} FUT", shown_date(e)),
            },
            _ if !underlying.is_empty() => underlying.to_string(),
            _ => trading.to_string(),
        };
        let id = get(token).to_string();
        out.push(Instrument {
            symbol: format!("{exchange}:{}", ticker_of(trading)),
            name: shown,
            kind: class.into(),
            key: id.clone(),
            id,
            segment: segment.into(),
            instrument: itype.into(),
            tick: get(tick).parse().unwrap_or(0.05),
            lot: get(lot).parse().unwrap_or(1.0),
            expiry,
            strike,
            option,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DUMP: &str = "instrument_token,exchange_token,tradingsymbol,name,last_price,expiry,strike,tick_size,lot_size,instrument_type,segment,exchange
738561,2885,RELIANCE,\"RELIANCE INDUSTRIES\",0,,0,0.05,1,EQ,NSE,NSE
256265,1001,NIFTY 50,\"NIFTY 50\",0,,0,0,0,EQ,INDICES,NSE
12345678,48226,NIFTY25NOV24000CE,\"NIFTY\",0,2025-11-27,24000,0.05,75,CE,NFO-OPT,NFO
13000002,50782,NIFTY25NOVFUT,\"NIFTY\",0,2025-11-27,0,0.1,75,FUT,NFO-FUT,NFO
999,9,USDINR25NOVFUT,\"USDINR\",0,2025-11-26,0,0.0025,1,FUT,CDS-FUT,CDS
5,5,SOMETHING,\"x\",0,,0,0.05,1,EQ,XX,NSEIX
";

    #[test]
    fn rows_become_the_apps_symbols() {
        let list = parse(DUMP).unwrap();
        let got: Vec<(&str, &str, &str)> = list.iter().map(|i| (i.symbol.as_str(), i.kind.as_str(), i.name.as_str())).collect();
        assert_eq!(
            got,
            [
                ("NSE:RELIANCE", "stock", "RELIANCE INDUSTRIES"),
                ("NSE:NIFTY_50", "index", "NIFTY 50"),
                ("NFO:NIFTY25NOV24000CE", "futures", "NIFTY 27/11/2025 24000 CE"),
                ("NFO:NIFTY25NOVFUT", "futures", "NIFTY 27/11/2025 FUT"),
                ("CDS:USDINR25NOVFUT", "futures", "USDINR 26/11/2025 FUT"),
            ]
        );
        let opt = &list[2];
        assert_eq!((opt.key.as_str(), opt.lot, opt.strike, opt.option.as_deref(), opt.expiry.as_deref()), ("12345678", 75.0, Some(24000.0), Some("CE"), Some("2025-11-27")));
        assert_eq!(list[4].pricescale(), 10_000);
    }

    #[test]
    fn a_gzipped_dump_is_read() {
        use std::io::Write;
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(DUMP.as_bytes()).unwrap();
        assert_eq!(text_of(gz.finish().unwrap()).unwrap(), DUMP);
    }
}
