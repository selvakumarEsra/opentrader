/*
 * The plugin between a fake app (the Plugin API's server side) and a fake OpenD (framed JSON): the hello, bars
 * asked of OpenD and stamped at their start, a subscription turned into quotes, symbol info, and the error when
 * OpenD is not running.
 */
use opentrader_moomoo::opend::crypt::{Aes, Rsa, Seal};
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
        isolate_store();
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

/// Each test run keeps its bars in a fresh directory, never the user's cache.
fn isolate_store() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let dir = std::env::temp_dir().join(format!("opentrader-moomoo-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // Set once, before any plugin starts.
        unsafe { std::env::set_var("OPENTRADER_MOOMOO_CACHE", dir) };
    });
}

fn ok(s2c: Value) -> Value {
    json!({"retType": 0, "retMsg": "", "s2c": s2c})
}

/// A fake OpenD on its own port: answers InitConnect, history, static info and subscriptions; after a
/// subscription it pushes one 1-minute K-line and one Basic quote of AAPL.
async fn fake_opend() -> String {
    fake_opend_sealed(None).await
}

/// The fake OpenD; with `pem`, encrypted as a real one holding that RSA key: RSA for InitConnect, then AES-CBC.
async fn fake_opend_sealed(pem: Option<String>) -> String {
    fake_opend_seen(pem, None).await
}

/// The fake OpenD, telling `seen` each request it gets (its proto and c2s).
async fn fake_opend_seen(pem: Option<String>, seen: Option<tokio::sync::mpsc::UnboundedSender<(u32, Value)>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut seal = Seal { rsa: pem.as_deref().map(|p| Rsa::from_pem(p).unwrap()), aes: None };
        let mut buf = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            while let Some(f) = frame::decode_with(&mut buf, |proto, body| seal.open(proto, body)).unwrap() {
                let encode = |proto: u32, serial: u32, v: &Value| {
                    let plain = serde_json::to_vec(v).unwrap();
                    frame::encode_sealed(proto, serial, &plain, &seal.close(proto, &plain).unwrap())
                };
                let req: Value = serde_json::from_slice(&f.body).unwrap();
                if let Some(seen) = &seen {
                    let _ = seen.send((f.proto, req["c2s"].clone()));
                }
                let mut out = Vec::new();
                let answer = match f.proto {
                    1001 => {
                        assert_eq!(req["c2s"]["packetEncAlgo"], if seal.is_on() { 2 } else { -1 });
                        ok(json!({"serverVer":1,"loginUserID":"1","connID":"1","connAESKey":"0123456789abcdef",
                            "aesCBCiv":"fedcba9876543210","keepAliveInterval":10}))
                    }
                    3103 => {
                        assert_eq!((req["c2s"]["klType"].as_i64(), req["c2s"]["security"]["code"].as_str()), (Some(1), Some("AAPL")));
                        assert_eq!(req["c2s"]["extendedTime"], true);
                        // End-stamped minutes of the extended US day, as some OpenD versions write them.
                        let k = |t: &str, c: f64| json!({"time":t,"isBlank":false,"openPrice":c,"highPrice":c,"lowPrice":c,"closePrice":c,"volume":"100"});
                        ok(json!({"security":{"market":11,"code":"AAPL"},
                            "klList":[k("2025-05-09 04:01:00", 1.0), k("2025-05-09 09:32:00", 2.0), k("2025-05-09 20:00:00", 3.0)]}))
                    }
                    3202 => ok(json!({"staticInfoList":[{"basic":{"security":{"market":11,"code":"AAPL"},"id":"1","lotSize":1,"secType":3,"name":"Apple Inc.","listTime":""}}]})),
                    3001 => {
                        let kl = json!({"rehabType":1,"klType":1,"security":{"market":11,"code":"AAPL"},
                            "klList":[{"time":"2025-05-09 10:00:00","isBlank":false,"openPrice":200,"highPrice":201,"lowPrice":199,"closePrice":200.5,"volume":"900"}]});
                        let basic = json!({"basicQotList":[{"security":{"market":11,"code":"AAPL"},"isSuspended":false,"listTime":"",
                            "priceSpread":0.01,"updateTime":"","highPrice":202,"openPrice":198,"lowPrice":197,"curPrice":200.5,
                            "lastClosePrice":196,"volume":"1000000","turnover":0,"turnoverRate":0,"amplitude":0,"updateTimestamp":1746799230.0}]});
                        out.extend(encode(3001, f.serial, &ok(json!({}))));
                        out.extend(encode(3007, 0, &ok(kl)));
                        out.extend(encode(3005, 0, &ok(basic)));
                        sock.write_all(&out).await.unwrap();
                        continue;
                    }
                    2001 => ok(json!({"accList":[{"trdEnv":0,"accID":"1","trdMarketAuthList":[2,1]}]})),
                    2101 => ok(json!({"funds":{"power":5000,"totalAssets":10000,"cash":9000,"marketVal":1000,"unrealizedPL":12.5,"realizedPL":0}})),
                    2102 => ok(json!({"positionList":[{"positionID":"55","positionSide":0,"code":"AAPL","secMarket":2,"qty":10,"costPrice":150,"price":160,"plVal":100}]})),
                    2202 => {
                        let c = &req["c2s"];
                        assert_eq!((c["trdSide"].as_i64(), c["orderType"].as_i64(), c["code"].as_str(), c["secMarket"].as_i64()), (Some(1), Some(11), Some("AAPL"), Some(2)));
                        assert_eq!((c["qty"].as_f64(), c["price"].as_f64(), c["auxPrice"].as_f64()), (Some(10.0), Some(101.0), Some(100.0)));
                        assert_eq!((c["header"]["accID"].as_str(), c["packetID"]["connID"].as_str()), (Some("1"), Some("1")));
                        // The order fills at once: OpenD pushes it.
                        let order = json!({"header":c["header"],"order":{"trdSide":1,"orderType":11,"orderStatus":11,"orderID":"88","code":"AAPL",
                            "secMarket":2,"qty":10,"price":101,"auxPrice":100,"fillQty":10,"fillAvgPrice":100.5,"updateTimestamp":1746799230.0}});
                        out.extend(encode(2202, f.serial, &ok(json!({"header":c["header"],"orderID":"88"}))));
                        out.extend(encode(2208, 0, &ok(order)));
                        sock.write_all(&out).await.unwrap();
                        continue;
                    }
                    _ => ok(json!({})),
                };
                out.extend(encode(f.proto, f.serial, &answer));
                if f.proto == 1001 && seal.is_on() {
                    seal.aes = Some(Aes::new(b"0123456789abcdef", Some(b"fedcba9876543210")).unwrap());
                }
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
    // 09:31 and 19:59 New York: the newest two of three end-stamped bars (04:01, 09:32, 20:00), moved back a minute.
    let times: Vec<f64> = bars.iter().map(|b| b[0].as_f64().unwrap()).collect();
    assert_eq!(times, [1_746_797_460.0, 1_746_835_140.0]);
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

fn write_key(name: &str) -> (String, String) {
    use rsa::pkcs1::EncodeRsaPrivateKey;
    let key = rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 1024).unwrap();
    let pem = key.to_pkcs1_pem(rsa::pkcs1::LineEnding::LF).unwrap().to_string();
    let path = std::env::temp_dir().join(format!("opentrader-moomoo-{name}-{}.pem", std::process::id()));
    std::fs::write(&path, &pem).unwrap();
    (pem, path.to_string_lossy().into_owned())
}

fn configure(host_port: &str, key: &str) -> Value {
    let (host, port) = host_port.split_once(':').unwrap();
    json!({"jsonrpc":"2.0","method":"plugin.configure","params":{"settings":{"host":host,"port":port,"rsaKeyFile":key}}})
}

#[tokio::test]
async fn with_the_rsa_key_the_session_is_encrypted() {
    let (pem, path) = write_key("good");
    let opend = fake_opend_sealed(Some(pem)).await;
    // Started at a closed port, then moved to the encrypted OpenD with its key.
    let closed = TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap().to_string();
    let mut app = FakeApp::start(closed).await;
    app.send(configure(&opend, &path)).await;
    let info = ask_until_connected(&mut app, "data.symbolInfo", json!({"ticker":"US:AAPL"})).await;
    assert_eq!(info["result"]["description"].as_str(), Some("Apple Inc."), "{info}");
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn an_unreadable_key_file_keeps_the_plugin_from_connecting_in_plain() {
    // A plain fake OpenD: a plain connection to it would answer.
    let opend = fake_opend().await;
    let closed = TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap().to_string();
    let mut app = FakeApp::start(closed).await;
    app.send(configure(&opend, "/nonexistent/opend-key.pem")).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    // Not in the US list file another test's session may have written: only OpenD could answer.
    let a = app.ask(5, "data.symbolInfo", json!({"ticker":"US:MSFT"})).await;
    assert!(a.get("error").is_some(), "{a}");
}

#[tokio::test]
async fn an_order_is_placed_and_its_fill_is_told() {
    let mut app = FakeApp::start(fake_opend().await).await;
    let state = loop {
        let m = app.next().await;
        if m["method"] == "broker.state" && m["params"]["connected"] == true {
            break m["params"].clone();
        }
    };
    let ids: Vec<&str> = state["accounts"].as_array().unwrap().iter().filter_map(|a| a["id"].as_str()).collect();
    assert_eq!(ids, ["SIM-1-US", "SIM-1-HK"]);
    assert_eq!((state["account"]["isDemo"].as_bool(), state["account"]["equity"].as_f64()), (Some(true), Some(10_000.0)));
    assert_eq!(state["positions"][0]["ticker"], "US:AAPL");

    app.send(json!({"jsonrpc":"2.0","id":7,"method":"broker.place","params":{"ticker":"US:AAPL","side":"buy","type":"stopLimit",
        "qty":10,"limitPrice":101,"stopPrice":100,"takeProfit":null,"stopLoss":null}})).await;
    // The fill's push may be told before the answer.
    let (mut answer, mut event) = (None, None);
    while answer.is_none() || event.is_none() {
        let m = app.next().await;
        if m["id"] == 7 {
            answer = Some(m);
        } else if m["method"] == "broker.event" {
            event = Some(m["params"].clone());
        }
    }
    let (a, event) = (answer.unwrap(), event.unwrap());
    assert_eq!(a["result"]["orderId"], "88", "{a}");
    assert_eq!((event["kind"].as_str(), event["price"].as_f64()), (Some("executed"), Some(100.5)));

    let refused = app.ask(8, "broker.place", json!({"ticker":"HK:00700","side":"buy","type":"market","qty":100})).await;
    assert!(refused["error"]["message"].as_str().unwrap().contains("does not trade"), "{refused}");
}

#[tokio::test]
async fn a_backfill_run_stores_opends_bars() {
    isolate_store();
    let args: Vec<String> = ["backfill", "US:AAPL", "--resolution", "1", "--from", "09/05/2025", "--opend"]
        .iter()
        .map(|s| s.to_string())
        .chain([fake_opend().await])
        .collect();
    let options = opentrader_moomoo::cli::parse(&args).unwrap();
    assert_eq!(opentrader_moomoo::cli::run(options).await, Ok(true));
}

#[tokio::test]
async fn always_record_subscribes_its_symbols_with_no_chart_open() {
    let (tx, mut seen) = tokio::sync::mpsc::unbounded_channel();
    let opend = fake_opend_seen(None, Some(tx)).await;
    let mut app = FakeApp::start(opend.clone()).await;
    let (host, port) = opend.split_once(':').unwrap();
    app.send(json!({"jsonrpc":"2.0","method":"plugin.configure","params":{"settings":{"host":host,"port":port,"alwaysRecord":"us:aapl, AAPL\nUS:AAPL"}}})).await;
    let sub = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            match seen.recv().await {
                Some((3001, c2s)) => return c2s,
                Some(_) => continue,
                None => panic!("OpenD closed"),
            }
        }
    })
    .await
    .expect("a subscription in time");
    assert_eq!(sub["securityList"], json!([{"market":11,"code":"AAPL"}]));
    assert_eq!((sub["isSubOrUnSub"].as_bool(), sub["subTypeList"].as_array().map(|t| t.contains(&json!(11)))), (Some(true), Some(true)));
}

#[tokio::test]
async fn the_us_list_is_loaded_once_and_kept_on_disk() {
    use opentrader_moomoo::search::{Catalog, US_LIST_FILE};
    let dir = std::env::temp_dir().join(format!("opentrader-moomoo-list-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (tx, mut seen) = tokio::sync::mpsc::unbounded_channel();
    let (client, _events) = opentrader_moomoo::opend::client::Client::new(fake_opend_seen(None, Some(tx)).await);
    tokio::spawn(client.clone().run());
    let now = 1_763_200_000;

    let catalog = Catalog::with_dir(Some(dir.clone()));
    let mut loaded = Err(String::new());
    for _ in 0..100 {
        loaded = catalog.keep_fresh(&client, now).await;
        if loaded.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(loaded, Ok(true));
    assert!(dir.join(US_LIST_FILE).is_file());
    assert_eq!(catalog.find("US:AAPL").await.map(|l| l.name), Some("Apple Inc.".to_string()));
    // Fresh: not asked again.
    while seen.try_recv().is_ok() {}
    assert_eq!(catalog.keep_fresh(&client, now + 3600).await, Ok(false));
    assert!(seen.try_recv().is_err(), "a fresh list asked OpenD");

    // A later run reads the file at start; a day later it loads it again.
    let later = Catalog::with_dir(Some(dir.clone()));
    assert_eq!(later.find("US:AAPL").await.map(|l| l.kind), Some("stock"));
    assert_eq!(later.keep_fresh(&client, now + 3600).await, Ok(false));
    assert_eq!(later.keep_fresh(&client, now + 2 * 86_400).await, Ok(true));
    let _ = std::fs::remove_dir_all(&dir);
}
