/*
 * The exchanges the plugin serves and OpenD's markets behind them: `US:AAPL` is OpenD's {market 11, "AAPL"}. Each
 * market has one time zone, regular session and tick, which `data.symbolInfo` reports and the bars are read in.
 */
use crate::opend::proto::Security;
use chrono_tz::Tz;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Market {
    Us,
    Hk,
    Sh,
    Sz,
}

impl Market {
    pub const ALL: [Market; 4] = [Market::Us, Market::Hk, Market::Sh, Market::Sz];

    pub fn exchange(self) -> &'static str {
        match self {
            Market::Us => "US",
            Market::Hk => "HK",
            Market::Sh => "SH",
            Market::Sz => "SZ",
        }
    }

    /// OpenD's QotMarket.
    pub fn qot(self) -> i32 {
        match self {
            Market::Us => 11,
            Market::Hk => 1,
            Market::Sh => 21,
            Market::Sz => 22,
        }
    }

    pub fn of_qot(market: i32) -> Option<Market> {
        Market::ALL.into_iter().find(|m| m.qot() == market)
    }

    pub fn of_exchange(exchange: &str) -> Option<Market> {
        Market::ALL.into_iter().find(|m| m.exchange().eq_ignore_ascii_case(exchange))
    }

    pub fn tz(self) -> Tz {
        match self {
            Market::Us => chrono_tz::America::New_York,
            Market::Hk => chrono_tz::Asia::Hong_Kong,
            Market::Sh | Market::Sz => chrono_tz::Asia::Shanghai,
        }
    }

    /// The regular session, in the datafeed grammar.
    pub fn session(self) -> &'static str {
        match self {
            Market::Us => "0930-1600",
            Market::Hk => "0930-1200,1300-1600",
            Market::Sh | Market::Sz => "0930-1130,1300-1500",
        }
    }

    /// Opens and closes of the regular session's parts, seconds after midnight.
    pub fn bounds(self) -> (&'static [u32], &'static [u32]) {
        const H: u32 = 3_600;
        match self {
            Market::Us => (&[9 * H + 1_800], &[16 * H]),
            Market::Hk => (&[9 * H + 1_800, 13 * H], &[12 * H, 16 * H]),
            Market::Sh | Market::Sz => (&[9 * H + 1_800, 13 * H], &[11 * H + 1_800, 15 * H]),
        }
    }

    pub fn currency(self) -> &'static str {
        match self {
            Market::Us => "USD",
            Market::Hk => "HKD",
            Market::Sh | Market::Sz => "CNY",
        }
    }

    /// Ticks per price unit: cents in US and China, a tenth of a cent in Hong Kong.
    pub fn pricescale(self) -> u32 {
        match self {
            Market::Hk => 1_000,
            _ => 100,
        }
    }
}

/// `US:AAPL` → (US, OpenD's security); `None` for another exchange or a bare ticker.
pub fn security_of(symbol: &str) -> Option<(Market, Security)> {
    let (exchange, code) = symbol.trim().split_once(':')?;
    let market = Market::of_exchange(exchange)?;
    let code = code.trim().to_ascii_uppercase();
    (!code.is_empty()).then(|| (market, Security { market: market.qot(), code }))
}

/// OpenD's security → the app's whole symbol.
pub fn symbol_of(sec: &Security) -> Option<String> {
    Market::of_qot(sec.market).map(|m| format!("{}:{}", m.exchange(), sec.code))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_symbols_both_ways() {
        for (sym, market) in [("US:AAPL", 11), ("HK:00700", 1), ("SH:600519", 21), ("SZ:000001", 22)] {
            let (_, sec) = security_of(sym).unwrap();
            assert_eq!(sec.market, market);
            assert_eq!(symbol_of(&sec).as_deref(), Some(sym));
        }
        assert_eq!(security_of("us:aapl").unwrap().1.code, "AAPL");
        assert!(security_of("XX:Y").is_none());
        assert!(security_of("AAPL").is_none());
        assert!(security_of("US:").is_none());
        assert!(symbol_of(&Security { market: 2, code: "X".into() }).is_none());
    }

    #[test]
    fn each_market_has_its_zone_and_session() {
        assert_eq!((Market::Us.tz().name(), Market::Us.session()), ("America/New_York", "0930-1600"));
        assert_eq!((Market::Hk.tz().name(), Market::Hk.session()), ("Asia/Hong_Kong", "0930-1200,1300-1600"));
        assert_eq!((Market::Sh.tz().name(), Market::Sz.session()), ("Asia/Shanghai", "0930-1130,1300-1500"));
    }
}
