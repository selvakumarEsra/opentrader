/*
 * Dhan's live market feed (wss://api-feed.dhan.co?version=2&token=&clientId=&authType=2): JSON subscriptions of
 * at most 100 instruments (`RequestCode` 17 quote, 18 unsubscribe), binary little-endian packets back, each an
 * 8-byte header (response code, message length int16, exchange segment, security id int32) and its fields:
 *   1 index, 2 ticker  LTP float32 @8, LTT int32 @12
 *   4 quote            LTP @8, LTQ int16 @12, LTT @14, ATP @18, volume int32 @22, sell qty @26, buy qty @30,
 *                      open @34, close @38 (after the close), high @42, low @46
 *   6 prev close       previous close float32 @8, previous OI int32 @12
 *   8 full             LTP @8, LTQ @12, LTT @14, ATP @18, volume @22, sell @26, buy @30, OI @34, OI high @38,
 *                      OI low @42, open @46, close @50, high @54, low @58, depth @62 (5 × 20 bytes)
 *   50 disconnect      reason int16 @8
 */
use crate::Dhan;
use opentrader_plugin_kit::catalog::Instrument;
use opentrader_plugin_kit::live::Tick;
use opentrader_plugin_kit::socket::{Broker, Closed};
use serde_json::{json, Value};
use std::collections::HashMap;
use tokio::sync::Notify;

#[derive(Debug, Clone, PartialEq)]
pub enum Packet {
    Trade { key: String, price: f64, time: i64, volume: Option<f64>, open: Option<f64>, high: Option<f64>, low: Option<f64> },
    PrevClose { key: String, close: f64 },
    Disconnect { reason: i64 },
}

fn f32_at(b: &[u8], at: usize) -> Option<f64> {
    b.get(at..at + 4).map(|x| f32::from_le_bytes([x[0], x[1], x[2], x[3]]) as f64)
}

fn i32_at(b: &[u8], at: usize) -> Option<i64> {
    b.get(at..at + 4).map(|x| i32::from_le_bytes([x[0], x[1], x[2], x[3]]) as i64)
}

/// A packet's whole size by its response code.
fn size_of(code: u8) -> Option<usize> {
    Some(match code {
        1 | 2 => 16,
        4 => 50,
        5 => 12,
        6 => 16,
        7 => 8,
        8 => 162,
        50 => 10,
        _ => return None,
    })
}

/// The packets of a binary message (one or more, back to back).
pub fn parse(msg: &[u8]) -> Vec<Packet> {
    let mut out = Vec::new();
    let mut at = 0;
    while at + 8 <= msg.len() {
        let b = &msg[at..];
        let code = b[0];
        let told = u16::from_le_bytes([b[1], b[2]]) as usize;
        let len = size_of(code).unwrap_or(told).max(8);
        if len > b.len() {
            break;
        }
        let p = &b[..len];
        let key = format!("{}|{}", p[3], i32_at(p, 4).unwrap_or(0));
        let positive = |v: Option<f64>| v.filter(|v| *v > 0.0);
        match code {
            1 | 2 => {
                if let (Some(price), Some(time)) = (f32_at(p, 8), i32_at(p, 12)) {
                    out.push(Packet::Trade { key, price, time, volume: None, open: None, high: None, low: None });
                }
            }
            4 => {
                if let (Some(price), Some(time)) = (f32_at(p, 8), i32_at(p, 14)) {
                    out.push(Packet::Trade {
                        key,
                        price,
                        time,
                        volume: i32_at(p, 22).map(|v| v as f64),
                        open: positive(f32_at(p, 34)),
                        high: positive(f32_at(p, 42)),
                        low: positive(f32_at(p, 46)),
                    });
                }
            }
            8 => {
                if let (Some(price), Some(time)) = (f32_at(p, 8), i32_at(p, 14)) {
                    out.push(Packet::Trade {
                        key,
                        price,
                        time,
                        volume: i32_at(p, 22).map(|v| v as f64),
                        open: positive(f32_at(p, 46)),
                        high: positive(f32_at(p, 54)),
                        low: positive(f32_at(p, 58)),
                    });
                }
            }
            6 => {
                if let Some(close) = f32_at(p, 8) {
                    out.push(Packet::PrevClose { key, close });
                }
            }
            50 => out.push(Packet::Disconnect { reason: p.get(8..10).map_or(0, |x| i16::from_le_bytes([x[0], x[1]]) as i64) }),
            _ => {}
        }
        at += len;
    }
    out
}

/// Dhan's trade time as UNIX seconds: some feeds stamp IST wall time as if UTC, which is 5 h 30 ahead of now.
pub fn unix_of(ltt: i64, now: i64) -> i64 {
    if ltt <= 0 {
        now
    } else if ltt > now + 3 * 3600 {
        ltt - 19_800
    } else {
        ltt
    }
}

/// The subscribe (17) or unsubscribe (18) messages of `list`, 100 instruments each.
pub fn requests(code: u8, list: &[&Instrument]) -> Vec<String> {
    list.chunks(100)
        .map(|chunk| {
            let items: Vec<Value> = chunk.iter().map(|i| json!({"ExchangeSegment": i.segment, "SecurityId": i.id})).collect();
            json!({"RequestCode": code, "InstrumentCount": items.len(), "InstrumentList": items}).to_string()
        })
        .collect()
}

/// Dhan's feed for the kit's stream loop: instruments are held by their segment and security id; the previous closes
/// it sends are kept for the ticks after them.
impl Broker for Dhan {
    const NAME: &'static str = "Dhan's feed";
    type State = HashMap<String, f64>;

    async fn token(&self) -> Result<String, String> {
        Dhan::token(self).await
    }

    async fn invalidate(&self, token: &str) {
        Dhan::invalidate(self, token).await
    }

    fn configured(&self) -> &Notify {
        &self.tokens.configured
    }

    fn url(&self, token: &str) -> String {
        format!("{}/?version=2&token={token}&clientId={}&authType=2", self.urls.feed.trim_end_matches('/'), self.client_id())
    }

    fn key(&self, inst: &Instrument) -> String {
        inst.key.clone()
    }

    fn subscribe(&self, new: &[&Instrument], gone: &[&Instrument]) -> Vec<String> {
        requests(18, gone).into_iter().chain(requests(17, new)).collect()
    }

    fn goodbye(&self) -> Vec<String> {
        vec![json!({"RequestCode": 12}).to_string()]
    }

    fn frame(
        &self,
        prev_close: &mut HashMap<String, f64>,
        frame: &[u8],
        held: &HashMap<String, Instrument>,
        now: i64,
        ticks: &mut Vec<Tick>,
    ) -> Result<(), Closed> {
        for p in parse(frame) {
            match p {
                Packet::PrevClose { key, close } => {
                    prev_close.insert(key, close);
                }
                Packet::Trade { key, price, time, volume, open, high, low } => {
                    let Some(inst) = held.get(&key) else { continue };
                    let prev_close = prev_close.get(&key).copied();
                    ticks.push(Tick { symbol: inst.symbol.clone(), time: unix_of(time, now), price, day_volume: volume, open, high, low, prev_close });
                }
                Packet::Disconnect { reason } => {
                    println!("[warn] Dhan closed the feed (code {reason})");
                    // 807: the access token ended.
                    return Err(if reason == 807 { Closed::TokenEnded } else { Closed::ByBroker });
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn header(code: u8, len: u16, segment: u8, id: i32) -> Vec<u8> {
        let mut h = vec![code];
        h.extend(len.to_le_bytes());
        h.push(segment);
        h.extend(id.to_le_bytes());
        h
    }

    pub fn quote(segment: u8, id: i32, price: f32, ltt: i32, volume: i32) -> Vec<u8> {
        let mut p = header(4, 50, segment, id);
        p.extend(price.to_le_bytes());
        p.extend(7i16.to_le_bytes());
        p.extend(ltt.to_le_bytes());
        p.extend(price.to_le_bytes()); // ATP
        p.extend(volume.to_le_bytes());
        p.extend(0i32.to_le_bytes());
        p.extend(0i32.to_le_bytes());
        for v in [1490.0f32, 0.0, 1510.0, 1485.0] {
            p.extend(v.to_le_bytes());
        }
        p
    }

    pub fn prev_close(segment: u8, id: i32, close: f32) -> Vec<u8> {
        let mut p = header(6, 16, segment, id);
        p.extend(close.to_le_bytes());
        p.extend(0i32.to_le_bytes());
        p
    }

    #[test]
    fn quote_and_prev_close_packets_read_back() {
        let mut msg = prev_close(1, 2885, 1480.0);
        msg.extend(quote(1, 2885, 1502.5, 1_763_005_530, 12_345));
        assert_eq!(
            parse(&msg),
            vec![
                Packet::PrevClose { key: "1|2885".into(), close: 1480.0 },
                Packet::Trade {
                    key: "1|2885".into(),
                    price: 1502.5,
                    time: 1_763_005_530,
                    volume: Some(12_345.0),
                    open: Some(1490.0),
                    high: Some(1510.0),
                    low: Some(1485.0)
                },
            ]
        );
    }

    #[test]
    fn ticker_and_disconnect_packets_and_ist_stamps() {
        let mut t = header(2, 16, 0, 13);
        t.extend(24_000.5f32.to_le_bytes());
        t.extend(1_763_005_530i32.to_le_bytes());
        let mut d = header(50, 10, 0, 0);
        d.extend(807i16.to_le_bytes());
        t.extend(d);
        let got = parse(&t);
        assert_eq!(got[0], Packet::Trade { key: "0|13".into(), price: 24_000.5, time: 1_763_005_530, volume: None, open: None, high: None, low: None });
        assert_eq!(got[1], Packet::Disconnect { reason: 807 });
        // An IST wall time read as UTC is brought back.
        assert_eq!(unix_of(1_763_005_530 + 19_800, 1_763_005_531), 1_763_005_530);
        assert_eq!(unix_of(1_763_005_530, 1_763_005_531), 1_763_005_530);
        assert_eq!(unix_of(0, 5), 5);
    }

    #[test]
    fn subscriptions_go_100_at_a_time() {
        let list: Vec<Instrument> = (0..150).map(|n| Instrument { id: n.to_string(), segment: "NSE_EQ".into(), ..Default::default() }).collect();
        let refs: Vec<&Instrument> = list.iter().collect();
        let msgs = requests(17, &refs);
        assert_eq!(msgs.len(), 2);
        let first: Value = serde_json::from_str(&msgs[0]).unwrap();
        assert_eq!((first["RequestCode"].as_u64(), first["InstrumentCount"].as_u64()), (Some(17), Some(100)));
        assert_eq!(first["InstrumentList"][0], json!({"ExchangeSegment":"NSE_EQ","SecurityId":"0"}));
    }
}
