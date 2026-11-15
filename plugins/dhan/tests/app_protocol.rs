/*
 * The plugin between a fake app and a fake Dhan (token, scrip master, charts and feed): the day's token from the
 * settings' DHAN_* credentials (PIN and TOTP), the scrip master in search, intraday bars with the token, and a
 * subscription streamed as quotes.
 */
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;

const TOTP: &str = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";

struct FakeApp {
    lines: Lines<BufReader<OwnedReadHalf>>,
    write: OwnedWriteHalf,
}

impl FakeApp {
    async fn start(urls: opentrader_dhan::auth::Urls) -> FakeApp {
        let dir = std::env::temp_dir().join(format!("opentrader-dhan-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        unsafe { std::env::set_var("OPENTRADER_DHAN_CACHE", &dir) };
        let app = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = app.local_addr().unwrap().to_string();
        tokio::spawn(async move { opentrader_plugin_kit::feed::run(&addr, "dhan", "tok", opentrader_dhan::Dhan::new(urls)).await });
        let (sock, _) = app.accept().await.unwrap();
        let (read, write) = sock.into_split();
        let mut me = FakeApp { lines: BufReader::new(read).lines(), write };
        let hello = me.next().await;
        assert_eq!(hello["method"], "hello");
        me.send(json!({"jsonrpc":"2.0","id":1,"result":{"app":"OpenTrader","api":2}})).await;
        me
    }

    async fn send(&mut self, msg: Value) {
        self.write.write_all(format!("{msg}\n").as_bytes()).await.unwrap();
    }

    async fn next(&mut self) -> Value {
        let line = tokio::time::timeout(Duration::from_secs(20), self.lines.next_line()).await.expect("a message in time");
        serde_json::from_str(&line.unwrap().expect("the plugin closed the connection")).unwrap()
    }

    async fn ask(&mut self, id: i64, method: &str, params: Value) -> Value {
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})).await;
        loop {
            let m = self.next().await;
            if m["id"] == id && m.get("method").is_none() {
                return m;
            }
        }
    }
}

struct Request {
    method: String,
    target: String,
    headers: HashMap<String, String>,
    body: String,
}

async fn read_request(sock: &mut BufReader<TcpStream>) -> Option<Request> {
    let mut line = String::new();
    if sock.read_line(&mut line).await.ok()? == 0 {
        return None;
    }
    let mut parts = line.split_whitespace();
    let (method, target) = (parts.next()?.to_string(), parts.next()?.to_string());
    let mut headers = HashMap::new();
    loop {
        let mut h = String::new();
        sock.read_line(&mut h).await.ok()?;
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let len = headers.get("content-length").and_then(|l| l.parse::<usize>().ok()).unwrap_or(0);
    let mut body = vec![0; len];
    sock.read_exact(&mut body).await.ok()?;
    Some(Request { method, target, headers, body: String::from_utf8_lossy(&body).into_owned() })
}

fn form(body: &str) -> HashMap<String, String> {
    body.split('&').filter_map(|kv| kv.split_once('=')).map(|(k, v)| (k.to_string(), v.replace('+', " "))).collect()
}

fn response(status: &str, extra: &[(&str, &str)], body: &str) -> String {
    let mut r = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: keep-alive\r\n", body.len());
    for (k, v) in extra {
        r.push_str(&format!("{k}: {v}\r\n"));
    }
    r.push_str("\r\n");
    r.push_str(body);
    r
}

#[derive(Default)]
struct Seen {
    tokens_made: usize,
    charts: Vec<(String, String, Value)>,
}

const MASTER: &str = "SEM_EXM_EXCH_ID,SEM_SEGMENT,SEM_SMST_SECURITY_ID,SEM_INSTRUMENT_NAME,SEM_EXPIRY_CODE,SEM_TRADING_SYMBOL,SEM_LOT_UNITS,SEM_CUSTOM_SYMBOL,SEM_EXPIRY_DATE,SEM_STRIKE_PRICE,SEM_OPTION_TYPE,SEM_TICK_SIZE,SEM_EXPIRY_FLAG,SEM_EXCH_INSTRUMENT_TYPE,SEM_SERIES,SM_SYMBOL_NAME
BSE,E,500325,EQUITY,0,RELIANCE,1.0,Reliance Industries,,,,5.0000,NA,ES,A,RELIANCE INDUSTRIES LTD.
NSE,E,2885,EQUITY,0,RELIANCE,1.0,Reliance Industries,,,,10.0000,NA,ES,EQ,RELIANCE INDUSTRIES LTD
NSE,I,13,INDEX,0,NIFTY,1.0,Nifty 50,0001-01-01,,XX,0.0500,,INDEX,X,NIFTY
";

/// A fake Dhan auth, API and scrip master on one port.
async fn fake_dhan(seen: Arc<Mutex<Seen>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        loop {
            let Ok((sock, _)) = listener.accept().await else { return };
            let seen = seen.clone();
            tokio::spawn(async move {
                let mut sock = BufReader::new(sock);
                while let Some(r) = read_request(&mut sock).await {
                    let path = r.target.split('?').next().unwrap_or("").to_string();
                    let out = match (r.method.as_str(), path.as_str()) {
                        ("POST", "/app/generateAccessToken") => {
                            let q = form(r.target.split_once('?').map_or("", |(_, q)| q));
                            let now = chrono::Utc::now().timestamp();
                            let ok = [now - 30, now, now + 30].iter().any(|t| opentrader_plugin_kit::totp::code(TOTP, *t).unwrap() == q["totp"]);
                            assert!(ok && q["dhanClientId"] == "1100" && q["pin"] == "123456", "{q:?}");
                            seen.lock().unwrap().tokens_made += 1;
                            response("200 OK", &[("Content-Type", "application/json")], r#"{"dhanClientId":"1100","accessToken":"DT","expiryTime":"2099-01-01T00:00:00.000"}"#)
                        }
                        ("GET", "/scrips.csv") => response("200 OK", &[("Content-Type", "text/csv")], MASTER),
                        ("POST", "/v2/charts/intraday") => {
                            let body: Value = serde_json::from_str(&r.body).unwrap();
                            seen.lock().unwrap().charts.push((
                                r.headers.get("access-token").cloned().unwrap_or_default(),
                                r.headers.get("client-id").cloned().unwrap_or_default(),
                                body,
                            ));
                            response(
                                "200 OK",
                                &[("Content-Type", "application/json")],
                                r#"{"open":[1500,1502],"high":[1505,1503],"low":[1499,1501],"close":[1502,1503],"volume":[1000,800],"timestamp":[1763005500,1763005560]}"#,
                            )
                        }
                        _ => response("404 Not Found", &[], "{}"),
                    };
                    if sock.get_mut().write_all(out.as_bytes()).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    base
}

fn le_quote(segment: u8, id: i32, price: f32, ltt: i32) -> Vec<u8> {
    let mut p = vec![4u8];
    p.extend(50u16.to_le_bytes());
    p.push(segment);
    p.extend(id.to_le_bytes());
    p.extend(price.to_le_bytes());
    p.extend(1i16.to_le_bytes());
    p.extend(ltt.to_le_bytes());
    p.extend(price.to_le_bytes());
    p.extend(99_999i32.to_le_bytes());
    p.extend(0i32.to_le_bytes());
    p.extend(0i32.to_le_bytes());
    for v in [1490.0f32, 0.0, 1510.0, 1485.0] {
        p.extend(v.to_le_bytes());
    }
    p
}

#[allow(clippy::result_large_err)]
/// A fake Dhan feed: checks the token, then answers a quote subscription with a prev-close and a quote packet.
async fn fake_feed(subscribed: Arc<Mutex<Vec<Value>>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        loop {
            let Ok((sock, _)) = listener.accept().await else { return };
            let subscribed = subscribed.clone();
            tokio::spawn(async move {
                let callback = |req: &tokio_tungstenite::tungstenite::handshake::server::Request, resp| {
                    let q = req.uri().query().unwrap_or("");
                    assert!(q.contains("version=2") && q.contains("token=DT") && q.contains("clientId=1100") && q.contains("authType=2"), "{q}");
                    Ok(resp)
                };
                let mut ws = tokio_tungstenite::accept_hdr_async(sock, callback).await.unwrap();
                while let Some(Ok(msg)) = ws.next().await {
                    if let Message::Text(t) = msg {
                        let v: Value = serde_json::from_str(&t).unwrap();
                        subscribed.lock().unwrap().push(v.clone());
                        if v["RequestCode"] == 17 {
                            let mut prev = vec![6u8];
                            prev.extend(16u16.to_le_bytes());
                            prev.push(1);
                            prev.extend(2885i32.to_le_bytes());
                            prev.extend(1480.0f32.to_le_bytes());
                            prev.extend(0i32.to_le_bytes());
                            ws.send(Message::binary(prev)).await.unwrap();
                            let now = chrono::Utc::now().timestamp() as i32;
                            ws.send(Message::binary(le_quote(1, 2885, 1502.5, now))).await.unwrap();
                        }
                    }
                }
            });
        }
    });
    url
}

#[tokio::test]
#[allow(clippy::result_large_err)]
async fn makes_a_token_lists_fetches_history_and_streams() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let subscribed = Arc::new(Mutex::new(Vec::new()));
    let base = fake_dhan(seen.clone()).await;
    let feed = fake_feed(subscribed.clone()).await;
    let urls = opentrader_dhan::auth::Urls { auth: base.clone(), api: base.clone(), feed, scrips: format!("{base}/scrips.csv") };
    let mut app = FakeApp::start(urls).await;

    app.send(json!({"jsonrpc":"2.0","method":"plugin.configure","params":{"settings":{"clientId":"1100","pin":"123456","totpSecret":TOTP}}})).await;
    let mut rows = Value::Null;
    for id in 3..60 {
        let a = app.ask(id, "data.search", json!({"query":"reliance"})).await;
        if a["result"]["symbols"].as_array().is_some_and(|s| !s.is_empty()) {
            rows = a["result"]["symbols"].clone();
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(rows, json!([
        {"ticker":"NSE:RELIANCE","description":"RELIANCE INDUSTRIES LTD","type":"stock"},
        {"ticker":"BSE:RELIANCE","description":"RELIANCE INDUSTRIES LTD.","type":"stock"}
    ]));
    let info = app.ask(70, "data.symbolInfo", json!({"ticker":"NSE:NIFTY"})).await;
    assert_eq!((info["result"]["type"].as_str(), info["result"]["description"].as_str()), (Some("index"), Some("Nifty 50")));
    assert_eq!(info["result"]["intervals"], json!(["1", "5", "15", "60", "1D"]));

    let (from, to) = (1_763_005_500, 1_763_005_800);
    let bars = app.ask(71, "data.bars", json!({"ticker":"NSE:RELIANCE","resolution":"1","from":from,"to":to,"countBack":10})).await;
    assert_eq!(bars["result"]["bars"], json!([[1_763_005_500.0, 1500.0, 1505.0, 1499.0, 1502.0, 1000.0], [1_763_005_560.0, 1502.0, 1503.0, 1501.0, 1503.0, 800.0]]), "{bars}");
    {
        let s = seen.lock().unwrap();
        assert_eq!(s.tokens_made, 1);
        let (token, client, body) = &s.charts[0];
        assert_eq!((token.as_str(), client.as_str()), ("DT", "1100"));
        assert_eq!(body, &json!({"securityId":"2885","exchangeSegment":"NSE_EQ","instrument":"EQUITY","interval":"1","oi":false,
            "fromDate":"2025-11-13 09:15:00","toDate":"2025-11-13 09:21:00"}));
    }

    app.send(json!({"jsonrpc":"2.0","method":"data.subscribe","params":{"tickers":["NSE:RELIANCE"]}})).await;
    let quote = loop {
        let m = app.next().await;
        if m["method"] == "data.quote" {
            break m["params"].clone();
        }
    };
    assert_eq!((quote["ticker"].as_str(), quote["price"].as_f64(), quote["prevClose"].as_f64()), (Some("NSE:RELIANCE"), Some(1502.5), Some(1480.0)));
    assert_eq!((quote["day"][1].as_f64(), quote["day"][2].as_f64(), quote["day"][5].as_f64()), (Some(1490.0), Some(1510.0), Some(99_999.0)));
    let subs = subscribed.lock().unwrap().clone();
    assert_eq!(subs, [json!({"RequestCode":17,"InstrumentCount":1,"InstrumentList":[{"ExchangeSegment":"NSE_EQ","SecurityId":"2885"}]})]);
}
