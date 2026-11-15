/*
 * The plugin between a fake app (the Plugin API's server side) and a fake OpenD (framed JSON): the hello, bars
 * asked of OpenD and stamped at their start, a subscription turned into quotes, symbol info, and the error when
 * OpenD is not running.
 */
use opentrader_moomoo::opend::frame;
use serde_json::{json, Value};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpListener;

struct FakeApp {
    lines: Lines<BufReader<OwnedReadHalf>>,
    write: OwnedWriteHalf,
}

impl FakeApp {
    /// Starts the plugin against this app and OpenD at `opend`, and answers its hello.
    async fn start(opend: String) -> FakeApp {
        let app = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = app.local_addr().unwrap().to_string();
        tokio::spawn(async move { opentrader_moomoo::run_session_with(&addr, "moomoo", "tok", &opend).await });
        let (sock, _) = app.accept().await.unwrap();
        let (read, write) = sock.into_split();
        let mut me = FakeApp { lines: BufReader::new(read).lines(), write };
        let hello = me.next().await;
        assert_eq!((hello["method"].as_str(), hello["params"]["api"].as_u64()), (Some("hello"), Some(2)));
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

fn ok(s2c: Value) -> Value {
    json!({"retType": 0, "retMsg": "", "s2c": s2c})
}

/// A fake OpenD on its own port: answers InitConnect, history, static info and subscriptions; after a
/// subscription it pushes one 1-minute K-line and one Basic quote of AAPL.
async fn fake_opend() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            while let Some(f) = frame::decode(&mut buf).unwrap() {
                let req: Value = serde_json::from_slice(&f.body).unwrap();
                let mut out = Vec::new();
                let answer = match f.proto {
                    1001 => {
                        assert_eq!(req["c2s"]["packetEncAlgo"], -1);
                        ok(json!({"serverVer":1,"loginUserID":"1","connID":"1","connAESKey":"0123456789abcdef","keepAliveInterval":10}))
                    }
                    3103 => {
                        assert_eq!((req["c2s"]["klType"].as_i64(), req["c2s"]["security"]["code"].as_str()), (Some(1), Some("AAPL")));
                        // End-stamped minutes, as some OpenD versions write them.
                        let k = |t: &str, c: f64| json!({"time":t,"isBlank":false,"openPrice":c,"highPrice":c,"lowPrice":c,"closePrice":c,"volume":"100"});
                        ok(json!({"security":{"market":11,"code":"AAPL"},
                            "klList":[k("2025-05-09 09:31:00", 1.0), k("2025-05-09 09:32:00", 2.0), k("2025-05-09 16:00:00", 3.0)]}))
                    }
                    3202 => ok(json!({"staticInfoList":[{"basic":{"security":{"market":11,"code":"AAPL"},"id":"1","lotSize":1,"secType":3,"name":"Apple Inc.","listTime":""}}]})),
                    3001 => {
                        let kl = json!({"rehabType":1,"klType":1,"security":{"market":11,"code":"AAPL"},
                            "klList":[{"time":"2025-05-09 10:00:00","isBlank":false,"openPrice":200,"highPrice":201,"lowPrice":199,"closePrice":200.5,"volume":"900"}]});
                        let basic = json!({"basicQotList":[{"security":{"market":11,"code":"AAPL"},"isSuspended":false,"listTime":"",
                            "priceSpread":0.01,"updateTime":"","highPrice":202,"openPrice":198,"lowPrice":197,"curPrice":200.5,
                            "lastClosePrice":196,"volume":"1000000","turnover":0,"turnoverRate":0,"amplitude":0,"updateTimestamp":1746799230.0}]});
                        out.extend(frame::encode(3001, f.serial, &serde_json::to_vec(&ok(json!({}))).unwrap()));
                        out.extend(frame::encode(3007, 0, &serde_json::to_vec(&ok(kl)).unwrap()));
                        out.extend(frame::encode(3005, 0, &serde_json::to_vec(&ok(basic)).unwrap()));
                        sock.write_all(&out).await.unwrap();
                        continue;
                    }
                    _ => ok(json!({})),
                };
                out.extend(frame::encode(f.proto, f.serial, &serde_json::to_vec(&answer).unwrap()));
                sock.write_all(&out).await.unwrap();
            }
            match sock.read(&mut chunk).await {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        }
    });
    addr
}

/// Asks until OpenD's connection is up (the plugin connects in the background).
async fn ask_until_connected(app: &mut FakeApp, method: &str, params: Value) -> Value {
    for id in 10.. {
        let a = app.ask(id, method, params.clone()).await;
        if a.get("error").is_none() || id > 60 {
            return a;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    unreachable!()
}

#[tokio::test]
async fn bars_come_from_opend_stamped_at_their_start() {
    let mut app = FakeApp::start(fake_opend().await).await;
    let from = 1_746_752_400.0; // 2025-05-09 01:00 New York
    let a = ask_until_connected(&mut app, "data.bars",
        json!({"ticker":"US:AAPL","resolution":"1","from":from,"to":from + 86_400.0,"countBack":2})).await;
    let bars = a["result"]["bars"].as_array().unwrap_or_else(|| panic!("{a}"));
    // 09:31 and 15:59 New York: the newest two of three end-stamped bars (09:31, 09:32, 16:00), moved back a minute.
    let times: Vec<f64> = bars.iter().map(|b| b[0].as_f64().unwrap()).collect();
    assert_eq!(times, [1_746_797_460.0, 1_746_820_740.0]);
    assert_eq!(bars[1][4].as_f64(), Some(3.0));
}

#[tokio::test]
async fn a_subscription_streams_quotes_and_symbol_info_names_the_stock() {
    let mut app = FakeApp::start(fake_opend().await).await;
    let info = ask_until_connected(&mut app, "data.symbolInfo", json!({"ticker":"US:AAPL"})).await;
    let r = &info["result"];
    assert_eq!((r["description"].as_str(), r["type"].as_str(), r["timezone"].as_str()), (Some("Apple Inc."), Some("stock"), Some("America/New_York")));
    assert_eq!(r["intervals"], json!(["1", "3", "5", "15", "30", "60", "1D"]));

    app.send(json!({"jsonrpc":"2.0","method":"data.subscribe","params":{"tickers":["US:AAPL"]}})).await;
    let quote = loop {
        let m = app.next().await;
        if m["method"] == "data.quote" {
            break m["params"].clone();
        }
    };
    assert_eq!((quote["ticker"].as_str(), quote["price"].as_f64(), quote["prevClose"].as_f64()), (Some("US:AAPL"), Some(200.5), Some(196.0)));
    assert_eq!(quote["day"][1].as_f64(), Some(198.0));
    assert_eq!(quote["minute"][4].as_f64(), Some(200.5));
}

#[tokio::test]
async fn without_opend_a_request_names_the_address() {
    // A port nothing listens on.
    let closed = TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap().to_string();
    let mut app = FakeApp::start(closed.clone()).await;
    let a = app.ask(2, "data.bars", json!({"ticker":"US:AAPL","resolution":"1","from":0,"to":1,"countBack":1})).await;
    assert_eq!(a["error"]["code"], -32000);
    assert!(a["error"]["message"].as_str().unwrap().contains(&closed), "{a}");
    let a = app.ask(3, "data.bars", json!({"ticker":"AAPL","resolution":"1","from":0,"to":1,"countBack":1})).await;
    assert_eq!(a["error"]["code"], -32602);
    let a = app.ask(4, "data.nope", json!({})).await;
    assert_eq!(a["error"]["code"], -32601);
}
