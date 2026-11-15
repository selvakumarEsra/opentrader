/*
 * Symbol identity — "EXCHANGE:TICKER", the full name the frontend keeps for
 * every chart, watchlist row and alert. Commands receive the full name and
 * hand providers both parts, so a provider listing the same ticker on two
 * exchanges (NSE:RELIANCE, BSE:RELIANCE) can tell them apart. Live events
 * carry the full name back.
 *
 * The fields are private: every `Symbol` went through the one normalisation
 * (trimmed, upper case), so two spellings of a symbol are always equal.
 */
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

/// One symbol: its exchange ("" for a bare ticker) and the provider ticker.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Symbol {
    exchange: String,
    ticker: String,
}

impl Symbol {
    /// "NASDAQ:AAPL" → (NASDAQ, AAPL); a bare "AAPL" → ("", AAPL).
    pub fn parse(symbol: &str) -> Self {
        let s = symbol.trim().to_uppercase();
        match s.split_once(':') {
            Some((exchange, ticker)) => Self { exchange: exchange.to_string(), ticker: ticker.to_string() },
            None => Self { exchange: String::new(), ticker: s },
        }
    }

    /// The same exchange with another ticker (a listing's earlier ticker).
    pub fn with_ticker(&self, ticker: &str) -> Self {
        Self { exchange: self.exchange.clone(), ticker: ticker.trim().to_uppercase() }
    }

    pub fn exchange(&self) -> &str {
        &self.exchange
    }

    pub fn ticker(&self) -> &str {
        &self.ticker
    }
}

/// The full name ("NASDAQ:AAPL"; the bare ticker when no exchange).
impl fmt::Display for Symbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.exchange.is_empty() {
            f.write_str(&self.exchange)?;
            f.write_str(":")?;
        }
        f.write_str(&self.ticker)
    }
}

impl Serialize for Symbol {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Symbol {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Self::parse(&String::deserialize(d)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_and_bare() {
        let s = Symbol::parse(" nse:reliance ");
        assert_eq!((s.exchange(), s.ticker()), ("NSE", "RELIANCE"));
        assert_eq!(s.to_string(), "NSE:RELIANCE");
        let b = Symbol::parse("aapl");
        assert_eq!((b.exchange(), b.ticker()), ("", "AAPL"));
        assert_eq!(b.to_string(), "AAPL");
    }

    #[test]
    fn reads_and_writes_its_full_name() {
        let s = Symbol::parse("NSE:RELIANCE");
        assert_eq!(serde_json::to_string(&s).unwrap(), r#""NSE:RELIANCE""#);
        assert_eq!(serde_json::from_str::<Symbol>(r#""nse:reliance""#).unwrap(), s);
    }

    #[test]
    fn another_ticker_keeps_the_exchange() {
        let s = Symbol::parse("NASDAQ:META").with_ticker(" fb");
        assert_eq!(s.to_string(), "NASDAQ:FB");
    }
}
