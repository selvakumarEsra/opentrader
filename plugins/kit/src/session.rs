/*
 * The connection to the app (plugins/PROTOCOL.md): `hello` with the plugin's id and token, then one JSON-RPC
 * message per line both ways. `connect` makes the hello; the plugin reads the returned lines and answers through
 * `Out`.
 */
use crate::rpc::Out;
use serde_json::{json, Value};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::net::tcp::OwnedReadHalf;
use tokio::net::TcpStream;

pub const API: u32 = 2;
const HELLO_TIMEOUT: Duration = Duration::from_secs(15);

pub type AppLines = Lines<BufReader<OwnedReadHalf>>;

/// Connects to the app at `addr` and says hello: the queue to the app and the app's lines, or why the app refused.
pub async fn connect(addr: &str, plugin: &str, token: &str, version: &str) -> Result<(Out, AppLines), String> {
    let stream = TcpStream::connect(addr).await.map_err(|e| format!("OpenTrader is not reachable at {addr}: {e}"))?;
    let _ = stream.set_nodelay(true);
    let (read, mut write) = stream.into_split();
    let (out, mut queue) = Out::channel();
    tokio::spawn(async move {
        while let Some(line) = queue.recv().await {
            if write.write_all(line.as_bytes()).await.is_err() {
                return;
            }
        }
    });
    let mut lines = BufReader::new(read).lines();
    out.send(json!({ "jsonrpc": "2.0", "id": 1, "method": "hello",
        "params": { "plugin": plugin, "token": token, "api": API, "version": version } }));
    let answer = tokio::time::timeout(HELLO_TIMEOUT, async {
        while let Ok(Some(line)) = lines.next_line().await {
            match serde_json::from_str::<Value>(&line) {
                Ok(msg) if msg.get("method").is_none() && msg.get("id") == Some(&json!(1)) => return Some(msg),
                _ => println!("[warn] ignored before hello: {line}"),
            }
        }
        None
    })
    .await
    .map_err(|_| "OpenTrader did not answer hello".to_string())?
    .ok_or("OpenTrader closed the connection during hello")?;
    if let Some(e) = answer.get("error") {
        let message = e.get("message").and_then(Value::as_str).unwrap_or("refused");
        return Err(format!("OpenTrader refused the plugin: {message}"));
    }
    println!("[info] connected to OpenTrader at {addr}");
    Ok((out, lines))
}

/// The app's address, the plugin's id and its token, from the environment the app starts it with; exits otherwise.
pub fn from_env(default_id: &str) -> (String, String, String) {
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let (Some(addr), Some(token)) = (env("OPENTRADER_PLUGIN_ADDR"), env("OPENTRADER_PLUGIN_TOKEN")) else {
        eprintln!("OPENTRADER_PLUGIN_ADDR and OPENTRADER_PLUGIN_TOKEN are not set: OpenTrader starts this program itself");
        std::process::exit(2);
    };
    (addr, env("OPENTRADER_PLUGIN_ID").unwrap_or_else(|| default_id.into()), token)
}
