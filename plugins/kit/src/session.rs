/*
 * The connection to the app (plugins/PROTOCOL.md): `hello` with the plugin's id and token, then one JSON-RPC
 * message per line both ways. `connect` makes the hello; `serve` hands the app's messages to the plugin's `Handler`
 * and answers through `Out`.
 *
 * Notifications (`plugin.configure`, `data.subscribe`) are handled one at a time, in the order the app sent them, so
 * a later one always wins. Requests run side by side (a long history does not hold a search up), each once every
 * notification sent before it has been handled: a `data.bars` right after `plugin.configure` sees its settings.
 */
use crate::rpc::{Out, RpcError, PARSE_ERROR};
use serde_json::{json, Value};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::net::tcp::OwnedReadHalf;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};

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

/// What a plugin does with the app's messages.
pub trait Handler: Send + Sync + 'static {
    /// A request: its answer.
    fn request(self: Arc<Self>, method: String, params: Value) -> impl Future<Output = Result<Value, RpcError>> + Send;
    /// A notification: nothing is answered.
    fn notification(self: Arc<Self>, method: String, params: Value) -> impl Future<Output = ()> + Send;
}

/// Serves the app's `lines` with `handler` until the app closes the connection, then waits for the notifications
/// already received to be handled.
pub async fn serve<H: Handler>(mut lines: AppLines, out: Out, handler: Arc<H>) {
    // Notifications are numbered as they come; `handled` is the number of those done.
    let (queue, mut queued) = mpsc::unbounded_channel::<(String, Value)>();
    let (done, handled) = watch::channel(0u64);
    let h = handler.clone();
    let notifications = tokio::spawn(async move {
        while let Some((method, params)) = queued.recv().await {
            h.clone().notification(method, params).await;
            done.send_modify(|n| *n += 1);
        }
    });
    let mut received = 0u64;
    while let Ok(Some(line)) = lines.next_line().await {
        let msg: Value = match serde_json::from_str(&line) {
            Ok(m) => m,
            Err(e) => {
                out.answer(Value::Null, Err(RpcError::new(PARSE_ERROR, format!("Parse error: {e}"))));
                continue;
            }
        };
        let Some(method) = msg.get("method").and_then(Value::as_str).map(str::to_string) else {
            continue;
        };
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        match msg.get("id").cloned() {
            Some(id) => {
                let (h, out, mut handled, before) = (handler.clone(), out.clone(), handled.clone(), received);
                tokio::spawn(async move {
                    let _ = handled.wait_for(|n| *n >= before).await;
                    out.answer(id, h.request(method, params).await);
                });
            }
            None => {
                received += 1;
                let _ = queue.send((method, params));
            }
        }
    }
    drop(queue);
    let _ = notifications.await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Records the order things are handled in; the first configure is slow.
    #[derive(Default)]
    struct Recorder(Mutex<Vec<String>>);

    impl Handler for Recorder {
        async fn request(self: Arc<Self>, method: String, _: Value) -> Result<Value, RpcError> {
            self.0.lock().unwrap().push(method);
            Ok(Value::Null)
        }
        async fn notification(self: Arc<Self>, method: String, params: Value) {
            if params == json!("slow") {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            self.0.lock().unwrap().push(format!("{method} {params}"));
        }
    }

    #[tokio::test]
    async fn notifications_keep_their_order_and_requests_wait_for_those_before_them() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let text = [
                json!({"jsonrpc":"2.0","method":"plugin.configure","params":"slow"}),
                json!({"jsonrpc":"2.0","method":"data.subscribe","params":"b"}),
                json!({"jsonrpc":"2.0","id":2,"method":"data.bars"}),
            ]
            .iter()
            .map(|m| format!("{m}\n"))
            .collect::<String>();
            s.write_all(text.as_bytes()).await.unwrap();
        });
        let stream = TcpStream::connect(addr).await.unwrap();
        let (read, _write) = stream.into_split();
        let (out, _queue) = Out::channel();
        let rec = Arc::new(Recorder::default());
        app.await.unwrap();
        serve(BufReader::new(read).lines(), out, rec.clone()).await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(*rec.0.lock().unwrap(), ["plugin.configure \"slow\"", "data.subscribe \"b\"", "data.bars"]);
    }
}
