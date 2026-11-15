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
use futures_util::{SinkExt, StreamExt};
use opentrader_plugin_kit::catalog::Instrument;
use opentrader_plugin_kit::feed::Event;
use opentrader_plugin_kit::live::Tick;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::Message;

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

/// The stream for the plugin's life: connected while there is something to stream and a token, again after a drop.
pub async fn run(z: Arc<Zerodha>, mut wanted: watch::Receiver<Vec<Instrument>>, events: mpsc::UnboundedSender<Event>) {
    let mut wait = Duration::from_secs(2);
    loop {
        if wanted.borrow().is_empty() {
            if wanted.changed().await.is_err() {
                return;
            }
            continue;
        }
        let token = match z.token().await {
            Ok(t) => t,
            Err(e) => {
                println!("[warn] live prices are off: {e}");
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(60)) => {}
                    _ = z.configured.notified() => {}
                }
                continue;
            }
        };
        let url = format!("{}/?api_key={}&access_token={}", z.urls.ws.trim_end_matches('/'), z.api_key(), token);
        let ws = match tokio_tungstenite::connect_async(url.as_str()).await {
            Ok((ws, _)) => ws,
            Err(e) => {
                let text = e.to_string();
                if text.contains("403") {
                    z.invalidate(&token).await;
                }
                println!("[warn] Kite stream: {}", text.replace(&token, "…"));
                tokio::time::sleep(wait).await;
                wait = (wait * 2).min(Duration::from_secs(60));
                continue;
            }
        };
        wait = Duration::from_secs(2);
        println!("[info] connected to Kite's stream");
        let _ = events.send(Event::Connected);
        let (mut tx, mut rx) = ws.split();
        let mut held: HashSet<u32> = HashSet::new();
        'session: loop {
            // The set to hold: subscribe the new ones in full mode, unsubscribe the dropped ones.
            let list = wanted.borrow_and_update().clone();
            let by_token: HashMap<u32, Instrument> = list.iter().filter_map(|i| i.id.parse::<u32>().ok().map(|t| (t, i.clone()))).collect();
            let want: HashSet<u32> = by_token.keys().copied().collect();
            let new: Vec<u32> = want.difference(&held).copied().collect();
            let gone: Vec<u32> = held.difference(&want).copied().collect();
            if !gone.is_empty() && tx.send(Message::text(json!({"a":"unsubscribe","v":gone}).to_string())).await.is_err() {
                break 'session;
            }
            if !new.is_empty() {
                let sent = tx.send(Message::text(json!({"a":"subscribe","v":new}).to_string())).await.is_ok()
                    && tx.send(Message::text(json!({"a":"mode","v":["full", new]}).to_string())).await.is_ok();
                if !sent {
                    break 'session;
                }
            }
            held = want;
            if held.is_empty() {
                let _ = tx.send(Message::Close(None)).await;
                break 'session;
            }
            loop {
                tokio::select! {
                    changed = wanted.changed() => {
                        if changed.is_err() { return; }
                        continue 'session;
                    }
                    msg = rx.next() => match msg {
                        Some(Ok(Message::Binary(frame))) => {
                            let now = chrono::Utc::now().timestamp();
                            for p in packets(&frame) {
                                let Some(packet) = parse(p, |t| by_token.get(&t).map_or(100.0, |i| divisor_of(&i.segment))) else { continue };
                                if let Some(inst) = by_token.get(&packet.token) {
                                    let _ = events.send(Event::Tick(tick_of(&packet, inst, now)));
                                }
                            }
                        }
                        Some(Ok(Message::Text(text))) => {
                            if text.contains("\"error\"") {
                                println!("[warn] Kite stream: {text}");
                            }
                        }
                        Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break 'session,
                        Some(Ok(_)) => {}
                    }
                }
            }
        }
        println!("[info] Kite's stream closed");
        tokio::time::sleep(wait).await;
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
        assert_eq!((i.price, i.high, i.low, i.open, i.close, i.time, i.volume), (24000.5, Some(24100.0), Some(23900.0), Some(23950.0), Some(23800.0), Some(1_763_017_240), None));
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
