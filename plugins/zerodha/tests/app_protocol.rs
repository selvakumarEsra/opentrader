/*
 * The plugin between a fake app and a fake Kite (login site, API and stream): the login from the settings' KITE_*
 * credentials (password, TOTP, request token, checksum), the instrument list in search, history bars with the
 * token, and a subscription streamed as quotes.
 */
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;

const KEY: &str = "kitekey";
const SECRET: &str = "kitesecret";
const TOTP: &str = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";

struct FakeApp {
    lines: Lines<BufReader<OwnedReadHalf>>,
    write: OwnedWriteHalf,
}

impl FakeApp {
    async fn start(urls: opentrader_zerodha::auth::Urls) -> FakeApp {
        let dir = std::env::temp_dir().join(format!("opentrader-zerodha-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        unsafe { std::env::set_var("OPENTRADER_ZERODHA_CACHE", &dir) };
        let app = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = app.local_addr().unwrap().to_string();
        tokio::spawn(async move { opentrader_plugin_kit::feed::run(&addr, "zerodha", "tok", opentrader_zerodha::Zerodha::new(urls)).await });
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

/// What the fake Kite saw.
#[derive(Default)]
struct Seen {
    history_auth: Vec<String>,
    history_targets: Vec<String>,
    logins: usize,
}

/// A fake Kite login site and API on one port.
async fn fake_kite(seen: Arc<Mutex<Seen>>) -> String {
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
                    let cookie = r.headers.get("cookie").cloned().unwrap_or_default();
                    let out = match (r.method.as_str(), path.as_str()) {
                        ("GET", "/connect/login") if r.target.contains("v=3") && !r.target.contains("sess_id") => {
                            response("302 Found", &[("Location", &format!("/connect/login?api_key={KEY}&sess_id=S1"))], "")
                        }
                        ("GET", "/connect/login") if cookie.contains("enctoken=E") => {
                            response("302 Found", &[("Location", "https://example.com/cb?action=login&status=success&request_token=RT")], "")
                        }
                        ("GET", "/connect/login") => response("200 OK", &[("Content-Type", "text/html")], "<html>login</html>"),
                        ("POST", "/api/login") => {
                            let f = form(&r.body);
                            assert_eq!((f["user_id"].as_str(), f["password"].as_str()), ("AB1234", "pw"));
                            seen.lock().unwrap().logins += 1;
                            response("200 OK", &[("Content-Type", "application/json")], r#"{"status":"success","data":{"request_id":"R1","twofa_type":"totp"}}"#)
                        }
                        ("POST", "/api/twofa") => {
                            let f = form(&r.body);
                            let now = chrono::Utc::now().timestamp();
                            let ok = [now - 30, now, now + 30].iter().any(|t| opentrader_plugin_kit::totp::code(TOTP, *t).unwrap() == f["twofa_value"]);
                            assert!(ok && f["request_id"] == "R1" && f["twofa_type"] == "totp", "{f:?}");
                            response("200 OK", &[("Content-Type", "application/json"), ("Set-Cookie", "enctoken=E; Path=/")], r#"{"status":"success","data":{}}"#)
                        }
                        ("POST", "/session/token") => {
                            let f = form(&r.body);
                            let sum: String = Sha256::digest(format!("{KEY}RT{SECRET}").as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
                            assert_eq!((f["api_key"].as_str(), f["request_token"].as_str(), f["checksum"].as_str()), (KEY, "RT", sum.as_str()));
                            response("200 OK", &[("Content-Type", "application/json")], r#"{"status":"success","data":{"access_token":"AT"}}"#)
                        }
                        ("GET", "/instruments") => response(
                            "200 OK",
                            &[("Content-Type", "text/csv")],
                            "instrument_token,exchange_token,tradingsymbol,name,last_price,expiry,strike,tick_size,lot_size,instrument_type,segment,exchange\n\
                             738561,2885,RELIANCE,\"RELIANCE INDUSTRIES\",0,,0,0.05,1,EQ,NSE,NSE\n\
                             256265,1001,NIFTY 50,\"NIFTY 50\",0,,0,0,0,EQ,INDICES,NSE\n",
                        ),
                        ("GET", p) if p.starts_with("/instruments/historical/738561/minute") => {
                            let mut s = seen.lock().unwrap();
                            s.history_auth.push(r.headers.get("authorization").cloned().unwrap_or_default());
                            s.history_targets.push(r.target.clone());
                            response(
                                "200 OK",
                                &[("Content-Type", "application/json")],
                                r#"{"status":"success","data":{"candles":[["2025-11-13T09:15:00+0530",1500,1505,1499,1502,1000],["2025-11-13T09:16:00+0530",1502,1503,1501,1503,800]]}}"#,
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

#[allow(clippy::result_large_err)]
/// A fake Kite stream: checks the token and the full-mode subscription, then sends one full packet of the token.
async fn fake_stream(subscribed: Arc<Mutex<Vec<Value>>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        loop {
            let Ok((sock, _)) = listener.accept().await else { return };
            let subscribed = subscribed.clone();
            tokio::spawn(async move {
                let callback = |req: &tokio_tungstenite::tungstenite::handshake::server::Request, resp| {
                    let q = req.uri().query().unwrap_or("");
                    assert!(q.contains(&format!("api_key={KEY}")) && q.contains("access_token=AT"), "{q}");
                    Ok(resp)
                };
                let mut ws = tokio_tungstenite::accept_hdr_async(sock, callback).await.unwrap();
                while let Some(Ok(msg)) = ws.next().await {
                    if let Message::Text(t) = msg {
                        let v: Value = serde_json::from_str(&t).unwrap();
                        subscribed.lock().unwrap().push(v.clone());
                        if v["a"] == "mode" {
                            let mut p = Vec::new();
                            let now = chrono::Utc::now().timestamp() as i32;
                            for x in [738_561, 150_250, 1, 150_000, 99_999, 0, 0, 149_000, 151_000, 148_500, 148_000, now, 0, 0, 0, now] {
                                p.extend(i32::to_be_bytes(x));
                            }
                            p.resize(184, 0);
                            let mut frame = 1u16.to_be_bytes().to_vec();
                            frame.extend((p.len() as u16).to_be_bytes());
                            frame.extend(p);
                            ws.send(Message::binary(frame)).await.unwrap();
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
async fn logs_in_lists_fetches_history_and_streams() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let subscribed = Arc::new(Mutex::new(Vec::new()));
    let base = fake_kite(seen.clone()).await;
    let ws = fake_stream(subscribed.clone()).await;
    let urls = opentrader_zerodha::auth::Urls { login: base.clone(), api: base, ws };
    let mut app = FakeApp::start(urls).await;

    // Without credentials, a search names what is missing in the log and finds nothing.
    let a = app.ask(2, "data.search", json!({"query":"reliance"})).await;
    assert_eq!(a["result"]["symbols"], json!([]));

    app.send(json!({"jsonrpc":"2.0","method":"plugin.configure","params":{"settings":{
        "apiKey": KEY, "apiSecret": SECRET, "userId": "AB1234", "password": "pw", "totpSecret": TOTP }}}))
    .await;
    let mut rows = Value::Null;
    for id in 3..60 {
        let a = app.ask(id, "data.search", json!({"query":"reliance"})).await;
        if a["result"]["symbols"].as_array().is_some_and(|s| !s.is_empty()) {
            rows = a["result"]["symbols"].clone();
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(rows[0], json!({"ticker":"NSE:RELIANCE","description":"RELIANCE INDUSTRIES","type":"stock"}));
    let info = app.ask(70, "data.symbolInfo", json!({"ticker":"NSE:RELIANCE"})).await;
    let r = &info["result"];
    assert_eq!((r["timezone"].as_str(), r["session"].as_str(), r["currency"].as_str(), r["pricescale"].as_u64()), (Some("Asia/Kolkata"), Some("0915-1530"), Some("INR"), Some(100)));

    // 2025-11-13 09:15 to 09:20 IST.
    let (from, to) = (1_763_005_500, 1_763_005_800);
    let bars = app.ask(71, "data.bars", json!({"ticker":"NSE:RELIANCE","resolution":"1","from":from,"to":to,"countBack":10})).await;
    assert_eq!(bars["result"]["bars"], json!([[1_763_005_500.0, 1500.0, 1505.0, 1499.0, 1502.0, 1000.0], [1_763_005_560.0, 1502.0, 1503.0, 1501.0, 1503.0, 800.0]]), "{bars}");
    {
        let s = seen.lock().unwrap();
        assert_eq!(s.history_auth, [format!("token {KEY}:AT")]);
        assert!(s.history_targets[0].contains("from=2025-11-13+09:15:00&to=2025-11-13+09:20:00"), "{:?}", s.history_targets);
        assert_eq!(s.logins, 1);
    }
    // Covered: asked again, the store answers.
    let again = app.ask(72, "data.bars", json!({"ticker":"NSE:RELIANCE","resolution":"1","from":from,"to":to,"countBack":10})).await;
    assert_eq!(again["result"]["bars"], bars["result"]["bars"]);
    assert_eq!(seen.lock().unwrap().history_targets.len(), 1);

    app.send(json!({"jsonrpc":"2.0","method":"data.subscribe","params":{"tickers":["NSE:RELIANCE"]}})).await;
    let quote = loop {
        let m = app.next().await;
        if m["method"] == "data.quote" {
            break m["params"].clone();
        }
    };
    assert_eq!((quote["ticker"].as_str(), quote["price"].as_f64(), quote["prevClose"].as_f64()), (Some("NSE:RELIANCE"), Some(1502.5), Some(1480.0)));
    assert_eq!(quote["day"][1].as_f64(), Some(1490.0));
    let subs = subscribed.lock().unwrap().clone();
    assert_eq!(subs, [json!({"a":"subscribe","v":[738561]}), json!({"a":"mode","v":["full",[738561]]})]);
}
