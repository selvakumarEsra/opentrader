/*
 * The broker side of the plugin (plugins/PROTOCOL.md, `broker`): the user's moomoo accounts through OpenD's trade
 * API. After each connection it lists the accounts and asks for their order and fill pushes; it sends the whole
 * `broker.state` after each push and every 5 s, and only when it changed. The plugin keeps no trade password: a
 * live account trades once the user has unlocked trading in OpenD.
 */
pub mod map;

use crate::opend::client::Client;
use crate::opend::proto;
use crate::rpc::{params, Out, RpcError, BAD_PARAMS};
use map::{AccountId, Snapshot, Ticket};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

pub const REFRESH_EVERY: Duration = Duration::from_secs(5);
/// TrdCategory: securities, not futures.
const CATEGORY_SECURITY: i32 = 1;

fn locked<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub struct Broker {
    client: Client,
    out: Out,
    accounts: Mutex<Vec<AccountId>>,
    selected: Mutex<Option<AccountId>>,
    /// The selected account's positions as OpenD last sent them, for closing one.
    positions: Mutex<Vec<Value>>,
    last_sent: Mutex<Option<Value>>,
    serial: AtomicU32,
}

#[derive(Deserialize)]
struct AccountParams {
    account: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OrderIdParams {
    order_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModifyParams {
    order_id: String,
    #[serde(flatten)]
    ticket: Ticket,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CloseParams {
    position_id: String,
    #[serde(default)]
    qty: Option<f64>,
}

#[derive(Deserialize)]
struct ResolveParams {
    #[serde(default)]
    symbol: String,
    #[serde(default)]
    ticker: String,
}

impl Broker {
    pub fn new(client: Client, out: Out) -> Broker {
        Broker {
            client,
            out,
            accounts: Mutex::new(Vec::new()),
            selected: Mutex::new(None),
            positions: Mutex::new(Vec::new()),
            last_sent: Mutex::new(None),
            serial: AtomicU32::new(0),
        }
    }

    /// A PacketID for an order: the connection's id and a number new on it.
    fn packet(&self) -> Result<Value, RpcError> {
        let login = self.client.login().ok_or_else(|| RpcError::from(format!("OpenD is not reachable at {}", self.client.addr())))?;
        let serial = self.serial.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
        Ok(json!({ "connID": login.conn_id.to_string(), "serialNo": serial }))
    }

    fn selected(&self) -> Result<AccountId, RpcError> {
        locked(&self.selected).ok_or_else(|| RpcError::from("No moomoo account is selected (is OpenD logged in?)".to_string()))
    }

    /// A new OpenD connection: the account list, their pushes, and the state.
    pub async fn connected(&self) {
        if let Err(e) = self.load_accounts().await {
            println!("[warn] moomoo accounts are not available: {e}");
            self.out.notify("broker.notice", json!({ "level": "warn", "message": format!("moomoo accounts are not available: {e}") }));
        }
        self.send_state().await;
    }

    async fn load_accounts(&self) -> Result<(), String> {
        let login = self.client.login().ok_or("OpenD is not connected")?;
        let s2c = self
            .client
            .request(proto::TRD_GET_ACC_LIST, json!({ "userID": login.user_id.to_string(), "trdCategory": CATEGORY_SECURITY }))
            .await?;
        let accounts = map::accounts_of(&s2c);
        let mut ids: Vec<String> = accounts.iter().map(|a| a.acc_id.to_string()).collect();
        ids.dedup();
        if !ids.is_empty() {
            self.client.request(proto::TRD_SUB_ACC_PUSH, json!({ "accIDList": ids })).await?;
        }
        {
            let mut selected = locked(&self.selected);
            if !selected.is_some_and(|s| accounts.contains(&s)) {
                *selected = accounts.first().copied();
            }
        }
        *locked(&self.accounts) = accounts;
        Ok(())
    }

    /// An order or fill push: its toast, then the new state.
    pub async fn push(&self, proto_id: u32, s2c: &Value) {
        let event = match proto_id {
            proto::TRD_UPDATE_ORDER => s2c.get("order").and_then(map::order_event),
            proto::TRD_UPDATE_ORDER_FILL => s2c.get("orderFill").and_then(map::fill_event),
            _ => None,
        };
        if let Some(e) = event {
            self.out.notify("broker.event", e);
        }
        self.send_state().await;
    }

    /// The whole state, sent when it differs from the last one sent.
    pub async fn send_state(&self) {
        let state = self.state().await;
        let mut last = locked(&self.last_sent);
        if last.as_ref() != Some(&state) {
            self.out.notify("broker.state", state.clone());
            *last = Some(state);
        }
    }

    async fn state(&self) -> Value {
        let accounts = locked(&self.accounts).clone();
        let selected = *locked(&self.selected);
        let connected = self.client.login().is_some() && selected.is_some();
        let mut snap = Snapshot { connected, accounts: &accounts, selected, funds: None, positions: vec![], orders: vec![], fills: vec![] };
        if let (true, Some(a)) = (connected, selected) {
            let header = json!({ "header": a.header() });
            let list = |s2c: Result<Value, String>, key: &str| -> Vec<Value> {
                s2c.ok().and_then(|v| v.get(key).and_then(Value::as_array).cloned()).unwrap_or_default()
            };
            let (funds, positions, orders, fills) = tokio::join!(
                self.client.request(proto::TRD_GET_FUNDS, header.clone()),
                self.client.request(proto::TRD_GET_POSITION_LIST, header.clone()),
                self.client.request(proto::TRD_GET_ORDER_LIST, header.clone()),
                // Paper accounts of some markets have no fill list: no executions then.
                self.client.request(proto::TRD_GET_ORDER_FILL_LIST, header),
            );
            snap.funds = funds.ok().and_then(|v| v.get("funds").cloned());
            snap.positions = list(positions, "positionList");
            snap.orders = list(orders, "orderList");
            snap.fills = list(fills, "orderFillList");
            *locked(&self.positions) = snap.positions.clone();
        }
        map::state_of(&snap)
    }

    pub async fn refresh_often(&self) {
        let mut tick = tokio::time::interval(REFRESH_EVERY);
        loop {
            tick.tick().await;
            self.send_state().await;
        }
    }

    /// A `broker.*` request; `None` for other methods.
    pub async fn request(&self, method: &str, p: Value) -> Option<Result<Value, RpcError>> {
        Some(match method {
            "broker.resolveSymbol" => params::<ResolveParams>(method, p).map(|p| json!({ "ticker": resolve(&p.symbol, &p.ticker) })),
            "broker.selectAccount" => self.select(method, p).await,
            "broker.place" => self.place(method, p).await,
            "broker.modify" => self.modify(method, p).await,
            "broker.cancel" => self.cancel(method, p).await,
            "broker.closePosition" => self.close(method, p).await,
            "broker.setPositionBrackets" => Err(RpcError::from("moomoo positions here have no take profit or stop loss: place them as separate orders".to_string())),
            _ => return None,
        })
    }

    async fn select(&self, method: &str, p: Value) -> Result<Value, RpcError> {
        let p: AccountParams = params(method, p)?;
        let id = AccountId::parse(&p.account).filter(|a| locked(&self.accounts).contains(a));
        let id = id.ok_or_else(|| RpcError::new(BAD_PARAMS, format!("No moomoo account {}", p.account)))?;
        *locked(&self.selected) = Some(id);
        self.send_state().await;
        Ok(json!({}))
    }

    async fn place(&self, method: &str, p: Value) -> Result<Value, RpcError> {
        let t: Ticket = params(method, p)?;
        self.place_ticket(&t).await
    }

    async fn place_ticket(&self, t: &Ticket) -> Result<Value, RpcError> {
        let account = self.selected()?;
        let c2s = map::order_request(&account, t, self.packet()?)?;
        let s2c = self.client.request(proto::TRD_PLACE_ORDER, c2s).await?;
        let id = s2c.get("orderID").map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_string)).unwrap_or_default();
        self.send_state().await;
        Ok(json!({ "orderId": id }))
    }

    async fn modify(&self, method: &str, p: Value) -> Result<Value, RpcError> {
        let p: ModifyParams = params(method, p)?;
        let account = self.selected()?;
        let c2s = map::modify_request(&account, order_id(&p.order_id)?, &p.ticket, self.packet()?)?;
        self.client.request(proto::TRD_MODIFY_ORDER, c2s).await?;
        self.send_state().await;
        Ok(json!({}))
    }

    async fn cancel(&self, method: &str, p: Value) -> Result<Value, RpcError> {
        let p: OrderIdParams = params(method, p)?;
        let account = self.selected()?;
        self.client.request(proto::TRD_MODIFY_ORDER, map::cancel_request(&account, order_id(&p.order_id)?, self.packet()?)).await?;
        self.send_state().await;
        Ok(json!({}))
    }

    async fn close(&self, method: &str, p: Value) -> Result<Value, RpcError> {
        let p: CloseParams = params(method, p)?;
        let position = locked(&self.positions)
            .iter()
            .find(|x| map::position_of(x, None).is_some_and(|v| v["id"] == p.position_id.as_str()))
            .cloned()
            .ok_or_else(|| RpcError::new(BAD_PARAMS, format!("No open position {}", p.position_id)))?;
        let t = map::closing_ticket(&position, p.qty)?;
        self.place_ticket(&t).await.map(|_| json!({}))
    }
}

fn order_id(s: &str) -> Result<u64, RpcError> {
    s.trim().parse().map_err(|_| RpcError::new(BAD_PARAMS, format!("{s} is not a moomoo order id")))
}

/// The plugin's ticker for a chart symbol of another data source: its own symbols as they are, US exchanges' stocks
/// as `US:<ticker>`, Hong Kong's as `HK:<code>`.
pub fn resolve(symbol: &str, ticker: &str) -> Option<String> {
    if crate::market::security_of(symbol).is_some() {
        return Some(symbol.to_string());
    }
    let exchange = symbol.split_once(':').map_or("", |(e, _)| e).to_ascii_uppercase();
    let ticker = ticker.trim();
    if ticker.is_empty() {
        return None;
    }
    let mapped = match exchange.as_str() {
        "NASDAQ" | "NYSE" | "AMEX" | "ARCA" | "NYSEARCA" | "BATS" | "CBOE" => format!("US:{ticker}"),
        "HKEX" => format!("HK:{ticker:0>5}"),
        "SSE" => format!("SH:{ticker}"),
        "SZSE" => format!("SZ:{ticker}"),
        _ => return None,
    };
    crate::market::security_of(&mapped).map(|_| mapped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chart_symbols_of_other_sources_resolve() {
        assert_eq!(resolve("US:AAPL", "AAPL").as_deref(), Some("US:AAPL"));
        assert_eq!(resolve("NASDAQ:AAPL", "AAPL").as_deref(), Some("US:AAPL"));
        assert_eq!(resolve("HKEX:700", "700").as_deref(), Some("HK:00700"));
        assert_eq!(resolve("FX:EURUSD", "EURUSD"), None);
    }
}
