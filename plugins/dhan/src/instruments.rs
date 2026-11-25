/*
 * Dhan's scrip master (the compact CSV, public): SEM_EXM_EXCH_ID, SEM_SEGMENT, SEM_SMST_SECURITY_ID,
 * SEM_INSTRUMENT_NAME, SEM_EXPIRY_CODE, SEM_TRADING_SYMBOL, SEM_LOT_UNITS, SEM_CUSTOM_SYMBOL, SEM_EXPIRY_DATE,
 * SEM_STRIKE_PRICE, SEM_OPTION_TYPE, SEM_TICK_SIZE, SEM_EXPIRY_FLAG, SEM_EXCH_INSTRUMENT_TYPE, SEM_SERIES,
 * SM_SYMBOL_NAME. Rows become `Instrument`s on the app's exchanges:
 *   NSE/BSE E (stocks ES, ETFs ETF)  NSE / BSE        Dhan segment NSE_EQ / BSE_EQ
 *   NSE/BSE I (indices)              NSE / BSE        IDX_I
 *   NSE/BSE D (futures, options)     NFO / BFO        NSE_FNO / BSE_FNO
 *   MCX M                            MCX              MCX_COMM
 *   NSE/BSE C (currency)             CDS / BCD        NSE_CURRENCY / BSE_CURRENCY
 * Tick sizes are in paise (indices' in rupees). The stream key is `<segment enum>|<security id>`.
 */
use opentrader_plugin_kit::catalog::{ticker_of, Instrument};

/// Dhan's exchange segment enum (the feed's numbers).
pub fn segment_number(segment: &str) -> Option<u8> {
    Some(match segment {
        "IDX_I" => 0,
        "NSE_EQ" => 1,
        "NSE_FNO" => 2,
        "NSE_CURRENCY" => 3,
        "BSE_EQ" => 4,
        "MCX_COMM" => 5,
        "BSE_CURRENCY" => 7,
        "BSE_FNO" => 8,
        _ => return None,
    })
}

pub fn segment_name(number: u8) -> Option<&'static str> {
    Some(match number {
        0 => "IDX_I",
        1 => "NSE_EQ",
        2 => "NSE_FNO",
        3 => "NSE_CURRENCY",
        4 => "BSE_EQ",
        5 => "MCX_COMM",
        7 => "BSE_CURRENCY",
        8 => "BSE_FNO",
        _ => return None,
    })
}

/// The stream key of a segment and security id.
pub fn key_of(segment: &str, security_id: &str) -> String {
    format!("{}|{security_id}", segment_number(segment).unwrap_or(255))
}

/// The app's exchange and Dhan's segment of an exchange id and segment letter.
fn place(exch: &str, seg: &str) -> Option<(&'static str, &'static str)> {
    Some(match (exch, seg) {
        ("NSE", "E") => ("NSE", "NSE_EQ"),
        ("BSE", "E") => ("BSE", "BSE_EQ"),
        ("NSE", "I") => ("NSE", "IDX_I"),
        ("BSE", "I") => ("BSE", "IDX_I"),
        ("NSE", "D") => ("NFO", "NSE_FNO"),
        ("BSE", "D") => ("BFO", "BSE_FNO"),
        ("MCX", "M") => ("MCX", "MCX_COMM"),
        ("NSE", "C") => ("CDS", "NSE_CURRENCY"),
        ("BSE", "C") => ("BCD", "BSE_CURRENCY"),
        _ => return None,
    })
}

const MONTHS: [&str; 12] = ["JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC"];

/// A derivative's ticker: `NIFTY-17NOV2026-24000-CE`, `BANKNIFTY-29DEC2026-FUT` (the underlying of Dhan's trading
/// symbol, the expiry date, the strike and the option type).
pub fn derivative_ticker(trading: &str, expiry: &str, strike: Option<f64>, option: Option<&str>) -> String {
    let underlying = trading.split('-').next().unwrap_or(trading).trim().to_ascii_uppercase();
    let p: Vec<&str> = expiry.split('-').collect();
    let month = p.get(1).and_then(|m| m.parse::<usize>().ok()).and_then(|m| MONTHS.get(m.wrapping_sub(1))).copied().unwrap_or("");
    let date = format!("{}{month}{}", p.get(2).unwrap_or(&""), p.first().unwrap_or(&""));
    match (option, strike.filter(|s| *s > 0.0)) {
        (Some(o), Some(s)) => {
            let s = if s.fract() == 0.0 { format!("{}", s as i64) } else { format!("{s}") };
            format!("{underlying}-{date}-{s}-{o}")
        }
        _ => format!("{underlying}-{date}-FUT"),
    }
}

/// The rows of a scrip master, without contracts expired before `today` (`YYYY-MM-DD`).
pub fn parse(csv_text: &str, today: &str) -> Result<Vec<Instrument>, String> {
    let mut reader = csv::ReaderBuilder::new().flexible(true).from_reader(csv_text.as_bytes());
    let headers = reader.headers().map_err(|e| format!("the scrip master: {e}"))?.clone();
    let col = |name: &str| headers.iter().position(|h| h.trim() == name);
    let need = |name: &str| col(name).ok_or_else(|| format!("the scrip master has no {name} column"));
    let (exch, seg, id, kind, symbol, lot, custom, expiry, strike, option, tick, itype, series, name) = (
        need("SEM_EXM_EXCH_ID")?,
        need("SEM_SEGMENT")?,
        need("SEM_SMST_SECURITY_ID")?,
        need("SEM_INSTRUMENT_NAME")?,
        need("SEM_TRADING_SYMBOL")?,
        need("SEM_LOT_UNITS")?,
        need("SEM_CUSTOM_SYMBOL")?,
        need("SEM_EXPIRY_DATE")?,
        need("SEM_STRIKE_PRICE")?,
        need("SEM_OPTION_TYPE")?,
        need("SEM_TICK_SIZE")?,
        need("SEM_EXCH_INSTRUMENT_TYPE")?,
        need("SEM_SERIES")?,
        need("SM_SYMBOL_NAME")?,
    );
    let mut out = Vec::new();
    for row in reader.records().flatten() {
        let get = |i: usize| row.get(i).unwrap_or("").trim();
        let Some((exchange, segment)) = place(get(exch), get(seg)) else { continue };
        let instrument = get(kind);
        let class = match (get(seg), instrument) {
            ("I", _) => "index",
            ("E", "EQUITY") => match get(itype) {
                "ES" => "stock",
                "ETF" => "fund",
                _ => continue,
            },
            ("E", _) => continue,
            _ => "futures",
        };
        // NSE cash: the equity series (bonds, gold bonds and bills are listed too).
        if segment == "NSE_EQ" && !matches!(get(series), "EQ" | "BE" | "BZ" | "SM" | "ST") {
            continue;
        }
        let expiry = Some(get(expiry).chars().take(10).collect::<String>()).filter(|e| e.len() == 10 && !e.starts_with("0001"));
        if class == "futures" && expiry.as_deref().is_some_and(|e| e < today) {
            continue;
        }
        let trading = get(symbol);
        if trading.is_empty() {
            continue;
        }
        let tick_given: f64 = get(tick).parse().unwrap_or(0.0);
        let tick = if class == "index" { tick_given } else { tick_given / 100.0 };
        let option = Some(get(option).to_string()).filter(|o| matches!(o.as_str(), "CE" | "PE"));
        let shown = match class {
            "stock" | "fund" if !get(name).is_empty() => get(name).to_string(),
            _ if !get(custom).is_empty() => get(custom).to_string(),
            _ => trading.to_string(),
        };
        let security = get(id).to_string();
        // Derivatives are named by their whole expiry date: Dhan's weekly options share a trading symbol.
        let ticker = match (class, expiry.as_deref()) {
            ("futures", Some(e)) => derivative_ticker(trading, e, get(strike).parse().ok(), option.as_deref()),
            _ => ticker_of(trading),
        };
        out.push(Instrument {
            symbol: format!("{exchange}:{ticker}"),
            name: shown,
            kind: class.into(),
            key: key_of(segment, &security),
            id: security,
            segment: segment.into(),
            instrument: instrument.into(),
            tick: if tick > 0.0 { tick } else { 0.05 },
            lot: get(lot).parse().unwrap_or(1.0),
            expiry: if class == "futures" { expiry } else { None },
            strike: get(strike).parse::<f64>().ok().filter(|s| *s > 0.0),
            option,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    pub const MASTER: &str = "SEM_EXM_EXCH_ID,SEM_SEGMENT,SEM_SMST_SECURITY_ID,SEM_INSTRUMENT_NAME,SEM_EXPIRY_CODE,SEM_TRADING_SYMBOL,SEM_LOT_UNITS,SEM_CUSTOM_SYMBOL,SEM_EXPIRY_DATE,SEM_STRIKE_PRICE,SEM_OPTION_TYPE,SEM_TICK_SIZE,SEM_EXPIRY_FLAG,SEM_EXCH_INSTRUMENT_TYPE,SEM_SERIES,SM_SYMBOL_NAME
NSE,E,2885,EQUITY,0,RELIANCE,1.0,Reliance Industries,,,,10.0000,NA,ES,EQ,RELIANCE INDUSTRIES LTD
NSE,E,10576,EQUITY,0,NIFTYBEES,1.0,Nippon Nifty 50 ETF (NIFTYBEES),,,,1.0000,NA,ETF,EQ,NIP IND ETF NIFTY BEES
NSE,E,9999,EQUITY,0,SGBNOV26,1.0,SGB,,,,1.0000,NA,DBT,SG,SGB
NSE,I,13,INDEX,0,NIFTY,1.0,Nifty 50,0001-01-01,,XX,0.0500,,INDEX,X,NIFTY
NSE,D,35000,OPTIDX,0,NIFTY-Nov2026-24000-CE,75.0,NIFTY 24 NOV 24000 CALL,2026-11-24 14:30:00,24000.00000,CE,5.0000,W,OP,,
NSE,D,35001,FUTIDX,0,NIFTY-Oct2026-FUT,75.0,NIFTY OCT FUT,2026-10-27 14:30:00,-0.01000,XX,10.0000,M,FUT,,
MCX,M,483080,FUTCOM,0,SILVERM-30Nov2026-FUT,1.0,SILVERM NOV FUT,2026-11-30 23:30:00,0.00000,XX,100.0000,M,FUTCOM,2,SILVERM
NSE,M,1,OPTFUT,0,X,1.0,X,2026-11-30 23:30:00,0,CE,1,M,X,,
";

    #[test]
    fn rows_become_the_apps_symbols() {
        let list = parse(MASTER, "2026-11-15").unwrap();
        let got: Vec<(&str, &str, &str, &str, &str)> =
            list.iter().map(|i| (i.symbol.as_str(), i.kind.as_str(), i.segment.as_str(), i.key.as_str(), i.name.as_str())).collect();
        assert_eq!(
            got,
            [
                ("NSE:RELIANCE", "stock", "NSE_EQ", "1|2885", "RELIANCE INDUSTRIES LTD"),
                ("NSE:NIFTYBEES", "fund", "NSE_EQ", "1|10576", "NIP IND ETF NIFTY BEES"),
                ("NSE:NIFTY", "index", "IDX_I", "0|13", "Nifty 50"),
                ("NFO:NIFTY-24NOV2026-24000-CE", "futures", "NSE_FNO", "2|35000", "NIFTY 24 NOV 24000 CALL"),
                ("MCX:SILVERM-30NOV2026-FUT", "futures", "MCX_COMM", "5|483080", "SILVERM NOV FUT"),
            ]
        );
        assert_eq!((list[0].tick, list[0].pricescale()), (0.1, 10));
        assert_eq!(list[2].tick, 0.05);
        let opt = &list[3];
        assert_eq!((opt.instrument.as_str(), opt.lot, opt.strike, opt.option.as_deref(), opt.expiry.as_deref()), ("OPTIDX", 75.0, Some(24000.0), Some("CE"), Some("2026-11-24")));
    }

    #[test]
    fn weekly_options_differ_by_their_expiry_date() {
        assert_eq!(derivative_ticker("NIFTY-Nov2026-24000-CE", "2026-11-17", Some(24000.0), Some("CE")), "NIFTY-17NOV2026-24000-CE");
        assert_eq!(derivative_ticker("NIFTY-Nov2026-24000-CE", "2026-11-23", Some(24000.0), Some("CE")), "NIFTY-23NOV2026-24000-CE");
        assert_eq!(derivative_ticker("USDINR-28Aug2024-FUT", "2024-08-28", Some(-0.01), None), "USDINR-28AUG2024-FUT");
        assert_eq!(derivative_ticker("X-Dec2026-82.5-PE", "2026-12-29", Some(82.5), Some("PE")), "X-29DEC2026-82.5-PE");
    }

    #[test]
    fn segment_numbers_round_trip() {
        for s in ["IDX_I", "NSE_EQ", "NSE_FNO", "NSE_CURRENCY", "BSE_EQ", "MCX_COMM", "BSE_CURRENCY", "BSE_FNO"] {
            assert_eq!(segment_number(s).and_then(segment_name), Some(s));
        }
    }
}
