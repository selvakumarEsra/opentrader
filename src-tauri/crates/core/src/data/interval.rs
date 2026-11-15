/*
 * Bar size — a unit and a multiplier, parsed once where it enters (a command's
 * arguments, a plugin's resolution list) instead of matched as a word
 * ("second", "minute", "day") wherever it is used. Weeks and months are not
 * here: the app builds them from days.
 */
use serde::{Deserialize, Serialize};
use std::fmt;
use std::num::NonZeroU32;
use std::str::FromStr;

/// The unit of a bar. On the wire: "second", "minute", "day".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "lowercase")]
pub enum BarUnit {
    Second,
    Minute,
    Day,
}

impl BarUnit {
    pub const fn secs(self) -> u64 {
        match self {
            BarUnit::Second => 1,
            BarUnit::Minute => 60,
            BarUnit::Day => 86_400,
        }
    }
}

/// `mult` bars of `unit`: 15 seconds, 5 minutes, 1 day.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Interval {
    unit: BarUnit,
    mult: NonZeroU32,
}

impl Interval {
    /// A multiplier of 0 is taken as 1.
    pub fn new(unit: BarUnit, mult: u32) -> Self {
        Self { unit, mult: NonZeroU32::new(mult).unwrap_or(NonZeroU32::MIN) }
    }

    pub fn seconds(mult: u32) -> Self {
        Self::new(BarUnit::Second, mult)
    }

    pub fn minutes(mult: u32) -> Self {
        Self::new(BarUnit::Minute, mult)
    }

    pub fn day() -> Self {
        Self::new(BarUnit::Day, 1)
    }

    pub fn unit(self) -> BarUnit {
        self.unit
    }

    pub fn mult(self) -> u32 {
        self.mult.get()
    }

    /// The bar's length in seconds.
    pub fn secs(self) -> u64 {
        self.unit.secs() * self.mult.get() as u64
    }

    /// Bars in one 24-hour day (1 for daily bars).
    pub fn bars_per_day(self) -> u64 {
        (86_400 / self.secs()).max(1)
    }

    /// The plugin resolution name: "15S", "5", "1D".
    pub fn resolution(self) -> String {
        self.to_string()
    }
}

impl fmt::Display for Interval {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.unit {
            BarUnit::Second => write!(f, "{}S", self.mult),
            BarUnit::Minute => write!(f, "{}", self.mult),
            BarUnit::Day => write!(f, "{}D", self.mult),
        }
    }
}

/// A resolution that is not a bar size this app serves.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unsupported resolution {0:?}")]
pub struct BadResolution(pub String);

impl FromStr for Interval {
    type Err = BadResolution;

    /// The plugin grammar: "<n>S" seconds, "<n>" minutes, "1D" a day.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bad = || BadResolution(s.to_string());
        let t = s.trim();
        let (digits, unit) = match t.chars().last() {
            Some('S' | 's') => (&t[..t.len() - 1], BarUnit::Second),
            Some('D' | 'd') => (&t[..t.len() - 1], BarUnit::Day),
            _ => (t, BarUnit::Minute),
        };
        let mult: NonZeroU32 = digits.parse().map_err(|_| bad())?;
        if unit == BarUnit::Day && mult.get() != 1 {
            return Err(bad());
        }
        Ok(Self { unit, mult })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_plugin_grammar() {
        assert_eq!("15S".parse(), Ok(Interval::seconds(15)));
        assert_eq!("15s".parse(), Ok(Interval::seconds(15)));
        assert_eq!("5".parse(), Ok(Interval::minutes(5)));
        assert_eq!("60".parse(), Ok(Interval::minutes(60)));
        assert_eq!("1D".parse(), Ok(Interval::day()));
        assert_eq!("1d".parse(), Ok(Interval::day()));
    }

    #[test]
    fn refuses_what_is_not_a_served_size() {
        for s in ["", "0", "0S", "abc", "1W", "1M", "2D", "S", "-5"] {
            assert!(s.parse::<Interval>().is_err(), "{s:?} should be refused");
        }
    }

    #[test]
    fn writes_back_what_it_reads() {
        for iv in [Interval::seconds(15), Interval::minutes(5), Interval::minutes(240), Interval::day()] {
            assert_eq!(iv.resolution().parse(), Ok(iv));
        }
        assert_eq!(Interval::seconds(15).resolution(), "15S");
        assert_eq!(Interval::minutes(5).resolution(), "5");
        assert_eq!(Interval::day().resolution(), "1D");
    }

    #[test]
    fn knows_its_length_and_bars_per_day() {
        assert_eq!((Interval::seconds(15).secs(), Interval::seconds(15).bars_per_day()), (15, 5_760));
        assert_eq!((Interval::minutes(5).secs(), Interval::minutes(5).bars_per_day()), (300, 288));
        assert_eq!((Interval::day().secs(), Interval::day().bars_per_day()), (86_400, 1));
        assert_eq!(Interval::minutes(0), Interval::minutes(1));
    }

    #[test]
    fn units_keep_their_wire_words() {
        let s = serde_json::to_string(&[BarUnit::Second, BarUnit::Minute, BarUnit::Day]).unwrap();
        assert_eq!(s, r#"["second","minute","day"]"#);
        assert!(serde_json::from_str::<BarUnit>(r#""weird""#).is_err());
    }
}
