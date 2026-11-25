/*
 * Kite's WebSocket stream (wss://ws.kite.trade?api_key=&access_token=): `{"a":"subscribe","v":[tokens]}` and
 * `{"a":"mode","v":["full",[tokens]]}`; binary frames of big-endian packets: a count (int16), then each packet's
 * length (int16) and bytes. Prices are int32 paise (÷100; currency ÷10,000,000, BSE currency ÷10,000).
 *   ltp 8 bytes          token, last price
 *   index 28 / 32 bytes  token, last, high, low, open, close, change, (exchange time)
 *   quote 44 / full 184  token, last, last qty, average, volume, buy qty, sell qty, open, high, low, close,
 *                        (last trade time, OI, OI high, OI low, exchange time, depth)
 * `close` is the previous session's close. One-byte frames are heartbeats.
 */
use crate::Zerodha;
use opentrader_plugin_kit::catalog::Instrument;
use opentrader_plugin_kit::live::Tick;
use opentrader_plugin_kit::socket::{Broker, Closed};
use serde_json::json;
use std::collections::HashMap;
use tokio::sync::Notify;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Packet {
    pub token: u32,
    pub price: f64,
    pub volume: Option<f64>,
    pub open: Option<f64>,
    pub high: Option<f64>,
    pub low: Option<f64>,
    pub close: Option<f64>,
    /// Last trade time, else exchange time (UNIX seconds), when given.
    pub time: Option<i64>,
}

/// The packets of a binary frame.
pub fn packets(frame: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    if frame.len() < 2 {
        return out;
    }
    let count = u16::from_be_bytes([frame[0], frame[1]]) as usize;
    let mut at = 2;
    for _ in 0..count {
        if at + 2 > frame.len() {
            break;
        }
        let len = u16::from_be_bytes([frame[at], frame[at + 1]]) as usize;
        at += 2;
        if at + len > frame.len() {
            break;
        }
        out.push(&frame[at..at + len]);
        at += len;
    }
    out
}

fn i32_at(p: &[u8], at: usize) -> Option<i64> {
    p.get(at..at + 4).map(|b| i32::from_be_bytes([b[0], b[1], b[2], b[3]]) as i64)
}

/// The price divisor of a Kite segment.
pub fn divisor_of(segment: &str) -> f64 {
    if segment.starts_with("CDS") {
        10_000_000.0
    } else if segment.starts_with("BCD") {
        10_000.0
    } else {
        100.0
    }
}

/// One packet; `divisor` from the token's segment.
pub fn parse(p: &[u8], divisor: impl Fn(u32) -> f64) -> Option<Packet> {
    let token = i32_at(p, 0)? as u32;
    let d = divisor(token);
    let price = |at: usize| i32_at(p, at).map(|v| v as f64 / d);
    let positive = |t: Option<i64>| t.filter(|t| *t > 0);
    Some(match p.len() {
        8 => Packet { token, price: price(4)?, ..Default::default() },
        28 | 32 => Packet {
            token,
            price: price(4)?,
            high: price(8),
            low: price(12),
            open: price(16),
            close: price(20),
            time: if p.len() == 32 { positive(i32_at(p, 28)) } else { None },
            volume: None,
        },
        n if n >= 44 => Packet {
            token,
            price: price(4)?,
            volume: i32_at(p, 16).map(|v| v as f64),
            open: price(28),
            high: price(32),
            low: price(36),
            close: price(40),
            time: if n >= 64 { positive(i32_at(p, 44)).or(positive(i32_at(p, 60))) } else { None },
        },
        _ => return None,
    })
}

/// A packet as the plugin's tick of `inst`.
pub fn tick_of(p: &Packet, inst: &Instrument, now: i64) -> Tick {
    let positive = |v: Option<f64>| v.filter(|v| *v > 0.0);
    Tick {
        symbol: inst.symbol.clone(),
        time: p.time.unwrap_or(now),
        price: p.price,
        day_volume: p.volume,
        open: positive(p.open),
        high: positive(p.high),
        low: positive(p.low),
        prev_close: positive(p.close),
    }
}

/// Kite's stream for the kit's stream loop: instruments are held by their token, in full mode.
impl Broker for Zerodha {
    const NAME: &'static str = "Kite's stream";
    type State = ();

    async fn token(&self) -> Result<String, String> {
        Zerodha::token(self).await
    }

    async fn invalidate(&self, token: &str) {
        Zerodha::invalidate(self, token).await
    }

    fn configured(&self) -> &Notify {
        &self.tokens.configured
    }

    fn url(&self, token: &str) -> String {
        format!("{}/?api_key={}&access_token={token}", self.urls.ws.trim_end_matches('/'), self.api_key())
    }

    fn refused(&self, error: &str) -> bool {
        error.contains("403")
    }

    fn key(&self, inst: &Instrument) -> String {
        inst.id.clone()
    }

    fn subscribe(&self, new: &[&Instrument], gone: &[&Instrument]) -> Vec<String> {
        let tokens = |list: &[&Instrument]| list.iter().filter_map(|i| i.id.parse::<u32>().ok()).collect::<Vec<u32>>();
        let (new, gone) = (tokens(new), tokens(gone));
        let mut out = Vec::new();
        if !gone.is_empty() {
            out.push(json!({"a":"unsubscribe","v":gone}).to_string());
        }
        if !new.is_empty() {
            out.push(json!({"a":"subscribe","v":new}).to_string());
            out.push(json!({"a":"mode","v":["full", new]}).to_string());
        }
        out
    }

    fn frame(&self, _: &mut (), frame: &[u8], held: &HashMap<String, Instrument>, now: i64, ticks: &mut Vec<Tick>) -> Result<(), Closed> {
        for p in packets(frame) {
            let inst_of = |t: u32| held.get(&t.to_string());
            let Some(packet) = parse(p, |t| inst_of(t).map_or(100.0, |i| divisor_of(&i.segment))) else { continue };
            if let Some(inst) = inst_of(packet.token) {
                ticks.push(tick_of(&packet, inst, now));
            }
        }
        Ok(())
    }

    fn text(&self, text: &str) {
        if text.contains("\"error\"") {
            println!("[warn] Kite stream: {text}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn be(v: i32) -> [u8; 4] {
        v.to_be_bytes()
    }

    pub fn full_packet(token: i32) -> Vec<u8> {
        let mut p = Vec::new();
        for v in [token, 250_050, 10, 249_900, 1_234_567, 5, 6, 248_000, 251_000, 247_500, 246_000, 1_763_017_230, 0, 0, 0, 1_763_017_231] {
            p.extend(be(v));
        }
        p.resize(184, 0);
        p
    }

    pub fn frame(packets: &[Vec<u8>]) -> Vec<u8> {
        let mut f = (packets.len() as u16).to_be_bytes().to_vec();
        for p in packets {
            f.extend((p.len() as u16).to_be_bytes());
            f.extend(p);
        }
        f
    }

    #[test]
    fn a_full_packet_reads_back() {
        let f = frame(&[full_packet(738_561)]);
        let ps = packets(&f);
        assert_eq!(ps.len(), 1);
        let p = parse(ps[0], |_| 100.0).unwrap();
        assert_eq!(
            p,
            Packet {
                token: 738_561,
                price: 2500.5,
                volume: Some(1_234_567.0),
                open: Some(2480.0),
                high: Some(2510.0),
                low: Some(2475.0),
                close: Some(2460.0),
                time: Some(1_763_017_230)
            }
        );
    }

    #[test]
    fn an_index_packet_and_an_ltp_packet_read_back() {
        let mut idx = Vec::new();
        for v in [256_265, 2_400_050, 2_410_000, 2_390_000, 2_395_000, 2_380_000, 20_050, 1_763_017_240] {
            idx.extend(be(v));
        }
        let mut ltp = be(5).to_vec();
        ltp.extend(be(12_345));
        let f = frame(&[idx, ltp]);
        let ps = packets(&f);
        let i = parse(ps[0], |_| 100.0).unwrap();
        assert_eq!(
            (i.price, i.high, i.low, i.open, i.close, i.time, i.volume),
            (24000.5, Some(24100.0), Some(23900.0), Some(23950.0), Some(23800.0), Some(1_763_017_240), None)
        );
        let l = parse(ps[1], |_| 10_000_000.0).unwrap();
        assert_eq!((l.token, l.price), (5, 0.0012345));
        // A heartbeat and a cut frame hold nothing.
        assert!(packets(&[0]).is_empty());
        assert_eq!(packets(&f[..f.len() - 3]).len(), 1);
    }

    #[test]
    fn a_packet_becomes_a_tick() {
        let inst = Instrument { symbol: "NSE:RELIANCE".into(), ..Default::default() };
        let p = parse(&full_packet(1), |_| 100.0).unwrap();
        let t = tick_of(&p, &inst, 9);
        assert_eq!((t.symbol.as_str(), t.time, t.price, t.day_volume, t.prev_close), ("NSE:RELIANCE", 1_763_017_230, 2500.5, Some(1_234_567.0), Some(2460.0)));
    }
}
