/*
 * One connection to OpenD, kept up for the whole session: connect, InitConnect, KeepAlive at the interval OpenD
 * asks for, and again 5 s after each loss (or at once when the user changes the address). Answers find their
 * request by serial number; pushes go out as events, after a `Connected` event that tells the stream to subscribe
 * again.
 */
use super::frame::{self, Frame};
use super::proto;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::OwnedReadHalf;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot, watch};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const RETRY: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
/// The SDK's own client version; OpenD checks it is not too old.
const CLIENT_VER: i32 = 300;

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// A new connection is ready: what was subscribed is gone with the old one.
    Connected,
    Push { proto: u32, s2c: Value },
}

type Answer = oneshot::Sender<Result<Value, String>>;

struct Inner {
    addr: watch::Sender<String>,
    link: Mutex<Option<mpsc::UnboundedSender<Vec<u8>>>>,
    pending: Mutex<HashMap<u32, Answer>>,
    serial: AtomicU32,
    events: mpsc::UnboundedSender<Event>,
}

#[derive(Clone)]
pub struct Client(Arc<Inner>);

fn locked<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Client {
    pub fn new(addr: String) -> (Client, mpsc::UnboundedReceiver<Event>) {
        let (events, rx) = mpsc::unbounded_channel();
        let inner = Inner {
            addr: watch::Sender::new(addr),
            link: Mutex::new(None),
            pending: Mutex::new(HashMap::new()),
            serial: AtomicU32::new(0),
            events,
        };
        (Client(Arc::new(inner)), rx)
    }

    pub fn addr(&self) -> String {
        self.0.addr.borrow().clone()
    }

    /// A new address: the connection moves to it at once.
    pub fn set_addr(&self, addr: String) {
        self.0.addr.send_if_modified(|a| (*a != addr).then(|| *a = addr).is_some());
    }

    /// `c2s` as message `proto`; the answer's `s2c`, or OpenD's reason when it refused.
    pub async fn request(&self, proto: u32, c2s: Value) -> Result<Value, String> {
        let link = locked(&self.0.link).clone().ok_or_else(|| self.unreachable())?;
        let serial = self.0.serial.fetch_add(1, Ordering::Relaxed).wrapping_add(1).max(1);
        let (tx, rx) = oneshot::channel();
        locked(&self.0.pending).insert(serial, tx);
        let body = serde_json::to_vec(&json!({ "c2s": c2s })).map_err(|e| e.to_string())?;
        if link.send(frame::encode(proto, serial, &body)).is_err() {
            locked(&self.0.pending).remove(&serial);
            return Err(self.unreachable());
        }
        match tokio::time::timeout(REQUEST_TIMEOUT, rx).await {
            Ok(Ok(answer)) => answer,
            Ok(Err(_)) => Err(self.unreachable()),
            Err(_) => {
                locked(&self.0.pending).remove(&serial);
                Err(format!("OpenD did not answer message {proto} in time"))
            }
        }
    }

    fn unreachable(&self) -> String {
        format!("OpenD is not reachable at {}", self.addr())
    }

    /// Keeps the connection up until the process ends.
    pub async fn run(self) {
        let mut down_logged = false;
        let mut addr_rx = self.0.addr.subscribe();
        loop {
            let addr = self.addr();
            match self.session(&addr, &mut addr_rx).await {
                Ok(()) => {
                    println!("[warn] the connection to OpenD at {addr} ended");
                    down_logged = false;
                }
                Err(e) if !down_logged => {
                    println!("[warn] {e}; trying again every {} s", RETRY.as_secs());
                    down_logged = true;
                }
                Err(_) => {}
            }
            tokio::select! {
                _ = tokio::time::sleep(RETRY) => {}
                _ = addr_rx.changed() => down_logged = false,
            }
        }
    }

    /// One connection: Ok when it was up and then lost, Err when it never came up.
    async fn session(&self, addr: &str, addr_rx: &mut watch::Receiver<String>) -> Result<(), String> {
        let stream = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr))
            .await
            .map_err(|_| format!("OpenD is not reachable at {addr}"))?
            .map_err(|e| format!("OpenD is not reachable at {addr}: {e}"))?;
        let _ = stream.set_nodelay(true);
        let (read, mut write) = stream.into_split();
        let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let writer = tokio::spawn(async move {
            while let Some(bytes) = rx.recv().await {
                if write.write_all(&bytes).await.is_err() {
                    return;
                }
            }
        });
        *locked(&self.0.link) = Some(tx);
        let me = self.clone();
        let mut reader = tokio::spawn(async move { me.read_loop(read).await });
        addr_rx.mark_unchanged();

        let init = self
            .request(proto::INIT_CONNECT, json!({
                "clientVer": CLIENT_VER, "clientID": "opentrader-moomoo", "recvNotify": false,
                "packetEncAlgo": proto::PACKET_ENC_NONE, "pushProtoFmt": frame::FORMAT_JSON,
                "programmingLanguage": "Rust",
            }))
            .await;
        let result = match init {
            Err(e) => Err(format!("OpenD at {addr} refused the connection: {e}")),
            Ok(s2c) => {
                let every = s2c.get("keepAliveInterval").and_then(Value::as_u64).unwrap_or(10).max(1);
                println!("[info] connected to OpenD at {addr}");
                let _ = self.0.events.send(Event::Connected);
                let mut beat = tokio::time::interval(Duration::from_secs(every));
                beat.tick().await;
                loop {
                    tokio::select! {
                        _ = &mut reader => break,
                        _ = addr_rx.changed() => break,
                        _ = beat.tick() => {
                            let me = self.clone();
                            let now = chrono::Utc::now().timestamp();
                            tokio::spawn(async move { me.request(proto::KEEP_ALIVE, json!({ "time": now })).await });
                        }
                    }
                }
                Ok(())
            }
        };
        *locked(&self.0.link) = None;
        writer.abort();
        reader.abort();
        // Whoever still waits learns the connection is gone (their sender is dropped).
        locked(&self.0.pending).clear();
        result
    }

    async fn read_loop(self, mut read: OwnedReadHalf) {
        let mut buf = Vec::with_capacity(64 << 10);
        let mut chunk = vec![0u8; 64 << 10];
        loop {
            loop {
                match frame::decode(&mut buf) {
                    Ok(Some(f)) => self.dispatch(f),
                    Ok(None) => break,
                    Err(e) => {
                        println!("[error] {e}");
                        return;
                    }
                }
            }
            match read.read(&mut chunk).await {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        }
    }

    fn dispatch(&self, f: Frame) {
        let answer = serde_json::from_slice::<Value>(&f.body)
            .map_err(|e| format!("OpenD sent unreadable message {}: {e}", f.proto))
            .and_then(|v| outcome(f.proto, v));
        if proto::is_push(f.proto) {
            if let Ok(s2c) = answer {
                let _ = self.0.events.send(Event::Push { proto: f.proto, s2c });
            }
        } else if let Some(tx) = locked(&self.0.pending).remove(&f.serial) {
            let _ = tx.send(answer);
        }
    }
}

/// A Response's `s2c` when `retType` is 0, else its `retMsg`.
pub fn outcome(proto: u32, v: Value) -> Result<Value, String> {
    if v.get("retType").and_then(Value::as_i64).unwrap_or(-400) != 0 {
        let msg = v.get("retMsg").and_then(Value::as_str).filter(|m| !m.is_empty());
        return Err(msg.map_or_else(|| format!("OpenD refused message {proto}"), str::to_string));
    }
    Ok(v.get("s2c").cloned().unwrap_or_else(|| json!({})))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_is_its_s2c_or_its_reason() {
        assert_eq!(outcome(1, json!({"retType":0,"s2c":{"a":1}})), Ok(json!({"a":1})));
        assert_eq!(outcome(1, json!({"retType":0})), Ok(json!({})));
        assert_eq!(outcome(3103, json!({"retType":-1,"retMsg":"quota exceeded"})), Err("quota exceeded".into()));
        assert_eq!(outcome(3103, json!({"retType":-1})), Err("OpenD refused message 3103".into()));
    }
}
