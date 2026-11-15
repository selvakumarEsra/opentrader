/*
 * The app's broker messages as OpenD's trade messages and back, with no I/O: accounts, the order ticket as
 * Trd_PlaceOrder / Trd_ModifyOrder, and OpenD's funds, positions, orders and fills as `broker.state`.
 *
 * An app account is one OpenD account in one market it may trade: `SIM-<id>-US` (paper) or `REAL-<id>-HK`. SH and
 * SZ stocks trade in the HKCC (Stock Connect) or CN market.
 */
use crate::market::Market;
use serde::Deserialize;
use serde_json::{json, Value};

pub const ENV_SIMULATE: i32 = 0;
pub const ENV_REAL: i32 = 1;

const SIDE_BUY: i32 = 1;
const SIDE_SELL: i32 = 2;
const SIDE_SELL_SHORT: i32 = 3;

const TYPE_LIMIT: i32 = 1;
const TYPE_MARKET: i32 = 2;
const TYPE_STOP: i32 = 10;
const TYPE_STOP_LIMIT: i32 = 11;

const TIF_DAY: i32 = 0;
const TIF_GTC: i32 = 1;

pub const MODIFY_NORMAL: i32 = 1;
pub const MODIFY_CANCEL: i32 = 2;

/// A trade market of OpenD (TrdMarket).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TrdMarket {
    Hk = 1,
    Us = 2,
    Cn = 3,
    Hkcc = 4,
}

impl TrdMarket {
    fn of(code: i64) -> Option<TrdMarket> {
        Some(match code {
            1 => TrdMarket::Hk,
            2 => TrdMarket::Us,
            3 => TrdMarket::Cn,
            4 => TrdMarket::Hkcc,
            _ => return None,
        })
    }

    fn name(self) -> &'static str {
        match self {
            TrdMarket::Hk => "HK",
            TrdMarket::Us => "US",
            TrdMarket::Cn => "CN",
            TrdMarket::Hkcc => "HKCC",
        }
    }

    fn parse(s: &str) -> Option<TrdMarket> {
        [TrdMarket::Hk, TrdMarket::Us, TrdMarket::Cn, TrdMarket::Hkcc].into_iter().find(|m| m.name() == s)
    }

    pub fn currency(self) -> &'static str {
        match self {
            TrdMarket::Us => "USD",
            TrdMarket::Hk | TrdMarket::Hkcc => "HKD",
            TrdMarket::Cn => "CNY",
        }
    }

    /// The stock markets this trade market trades.
    fn trades(self, m: Market) -> bool {
        match self {
            TrdMarket::Us => m == Market::Us,
            TrdMarket::Hk => m == Market::Hk,
            TrdMarket::Cn | TrdMarket::Hkcc => matches!(m, Market::Sh | Market::Sz),
        }
    }
}

/// One app account: an OpenD account in one of its markets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AccountId {
    pub real: bool,
    pub acc_id: u64,
    pub market: TrdMarket,
}

impl AccountId {
    pub fn parse(s: &str) -> Option<AccountId> {
        let mut parts = s.split('-');
        let real = match parts.next()? {
            "SIM" => false,
            "REAL" => true,
            _ => return None,
        };
        let acc_id = parts.next()?.parse().ok()?;
        let market = TrdMarket::parse(parts.next()?)?;
        parts.next().is_none().then_some(AccountId { real, acc_id, market })
    }

    pub fn name(&self) -> String {
        format!("{} {} {}", if self.real { "Live" } else { "Paper" }, self.market.name(), self.acc_id)
    }

    /// Trd_Common's TrdHeader.
    pub fn header(&self) -> Value {
        json!({ "trdEnv": if self.real { ENV_REAL } else { ENV_SIMULATE }, "accID": self.acc_id.to_string(), "trdMarket": self.market as i32 })
    }
}

impl std::fmt::Display for AccountId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}-{}-{}", if self.real { "REAL" } else { "SIM" }, self.acc_id, self.market.name())
    }
}

fn u64_of(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| v.as_str()?.parse().ok())
}

fn f64_of(v: &Value, key: &str) -> Option<f64> {
    let x = v.get(key)?;
    x.as_f64().or_else(|| x.as_str()?.parse().ok())
}

/// Trd_GetAccList's accounts, one per market each may trade; paper accounts first.
pub fn accounts_of(s2c: &Value) -> Vec<AccountId> {
    let mut out: Vec<AccountId> = Vec::new();
    for acc in s2c.get("accList").and_then(Value::as_array).into_iter().flatten() {
        let Some(acc_id) = acc.get("accID").and_then(u64_of) else { continue };
        let real = acc.get("trdEnv").and_then(Value::as_i64) == Some(ENV_REAL as i64);
        for m in acc.get("trdMarketAuthList").and_then(Value::as_array).into_iter().flatten() {
            if let Some(market) = m.as_i64().and_then(TrdMarket::of) {
                let id = AccountId { real, acc_id, market };
                if !out.contains(&id) {
                    out.push(id);
                }
            }
        }
    }
    out.sort_by_key(|a| a.real);
    out
}

/// What the order ticket sends: `broker.place`, and `broker.modify` with `orderId`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ticket {
    pub ticker: String,
    pub side: String,
    #[serde(rename = "type")]
    pub order_type: String,
    pub qty: f64,
    #[serde(default)]
    pub limit_price: Option<f64>,
    #[serde(default)]
    pub stop_price: Option<f64>,
    #[serde(default)]
    pub take_profit: Option<f64>,
    #[serde(default)]
    pub stop_loss: Option<f64>,
    /// "day" (the default) or "gtc".
    #[serde(default)]
    pub duration: Option<String>,
}

/// OpenD's order type, price and aux (stop) price of a ticket.
fn priced(t: &Ticket) -> Result<(i32, f64, f64), String> {
    let need = |p: Option<f64>, what: &str| p.filter(|x| x.is_finite() && *x > 0.0).ok_or_else(|| format!("A {} order needs a {what} price", t.order_type));
    Ok(match t.order_type.as_str() {
        "market" => (TYPE_MARKET, 0.0, 0.0),
        "limit" => (TYPE_LIMIT, need(t.limit_price, "limit")?, 0.0),
        "stop" => (TYPE_STOP, 0.0, need(t.stop_price, "stop")?),
        "stopLimit" => (TYPE_STOP_LIMIT, need(t.limit_price, "limit")?, need(t.stop_price, "stop")?),
        other => return Err(format!("moomoo has no {other} orders here")),
    })
}

fn check(account: &AccountId, t: &Ticket) -> Result<(String, Market), String> {
    if t.take_profit.is_some() || t.stop_loss.is_some() {
        return Err("moomoo orders here have no take profit or stop loss: place them as separate orders".into());
    }
    if !(t.qty.is_finite() && t.qty > 0.0) {
        return Err("The quantity must be more than 0".into());
    }
    let (market, sec) = crate::market::security_of(&t.ticker).ok_or_else(|| format!("{} is not a moomoo symbol", t.ticker))?;
    if !account.market.trades(market) {
        return Err(format!("The account {} does not trade {}", account.name(), t.ticker));
    }
    Ok((sec.code, market))
}

/// OpenD's security market (TrdSecMarket) of a stock market.
fn sec_market(m: Market) -> i32 {
    match m {
        Market::Hk => 1,
        Market::Us => 2,
        Market::Sh => 31,
        Market::Sz => 32,
    }
}

fn market_of_sec(code: i64) -> Option<Market> {
    Some(match code {
        1 => Market::Hk,
        2 => Market::Us,
        31 => Market::Sh,
        32 => Market::Sz,
        _ => return None,
    })
}

/// Trd_PlaceOrder's request for `t` on `account`, or why it cannot be placed. `packet` is the PacketID.
pub fn order_request(account: &AccountId, t: &Ticket, packet: Value) -> Result<Value, String> {
    let (code, market) = check(account, t)?;
    let side = match t.side.as_str() {
        "buy" => SIDE_BUY,
        "sell" => SIDE_SELL,
        other => return Err(format!("The side {other} is not buy or sell")),
    };
    let (order_type, price, aux) = priced(t)?;
    let tif = match t.duration.as_deref() {
        None | Some("day") => TIF_DAY,
        Some("gtc") => TIF_GTC,
        Some(other) => return Err(format!("moomoo has no {other} orders")),
    };
    let mut c2s = json!({
        "packetID": packet, "header": account.header(), "trdSide": side, "orderType": order_type, "code": code,
        "qty": t.qty, "price": price, "secMarket": sec_market(market), "timeInForce": tif,
    });
    if order_type == TYPE_STOP || order_type == TYPE_STOP_LIMIT {
        c2s["auxPrice"] = json!(aux);
    }
    // Pre- and post-market for US orders.
    if market == Market::Us && order_type != TYPE_MARKET {
        c2s["fillOutsideRTH"] = json!(true);
    }
    Ok(c2s)
}

/// Trd_ModifyOrder's request changing order `order_id` to `t`.
pub fn modify_request(account: &AccountId, order_id: u64, t: &Ticket, packet: Value) -> Result<Value, String> {
    check(account, t)?;
    let (order_type, price, aux) = priced(t)?;
    let mut c2s = json!({
        "packetID": packet, "header": account.header(), "orderID": order_id.to_string(), "modifyOrderOp": MODIFY_NORMAL,
        "qty": t.qty, "price": price,
    });
    if order_type == TYPE_STOP || order_type == TYPE_STOP_LIMIT {
        c2s["auxPrice"] = json!(aux);
    }
    Ok(c2s)
}

/// Trd_ModifyOrder's request cancelling order `order_id`.
pub fn cancel_request(account: &AccountId, order_id: u64, packet: Value) -> Value {
    json!({ "packetID": packet, "header": account.header(), "orderID": order_id.to_string(), "modifyOrderOp": MODIFY_CANCEL, "qty": 0, "price": 0 })
}

/// The market order that closes `qty` (all when `None`) of `position`.
pub fn closing_ticket(position: &Value, qty: Option<f64>) -> Result<Ticket, String> {
    let p = position_of(position, None).ok_or("OpenD sent a position the plugin cannot read")?;
    let held = p["qty"].as_f64().unwrap_or(0.0);
    let qty = qty.unwrap_or(held);
    if !(qty > 0.0 && qty <= held) {
        return Err(format!("The position holds {held}; {qty} cannot be closed"));
    }
    let side = if p["side"] == "buy" { "sell" } else { "buy" };
    Ok(Ticket {
        ticker: p["ticker"].as_str().unwrap_or_default().to_string(),
        side: side.into(),
        order_type: "market".into(),
        qty,
        limit_price: None,
        stop_price: None,
        take_profit: None,
        stop_loss: None,
        duration: None,
    })
}

/// The app's order status of OpenD's OrderStatus.
pub fn status_of(status: i64) -> &'static str {
    match status {
        0..=2 => "placing",
        3 | 4 | 21 | 22 => "rejected",
        5 | 10 | 12 | 13 => "working",
        11 => "filled",
        14 | 15 | 23 | 24 => "cancelled",
        _ => "inactive",
    }
}

fn side_of(trd_side: i64) -> &'static str {
    if trd_side == SIDE_SELL as i64 || trd_side == SIDE_SELL_SHORT as i64 {
        "sell"
    } else {
        "buy"
    }
}

fn type_of(order_type: i64) -> &'static str {
    match order_type as i32 {
        TYPE_MARKET => "market",
        TYPE_STOP => "stop",
        TYPE_STOP_LIMIT => "stopLimit",
        _ => "limit",
    }
}

/// The app's ticker of an OpenD code, from its security market or else the account's.
fn ticker_of(v: &Value, account: Option<&AccountId>) -> Option<String> {
    let code = v.get("code")?.as_str()?;
    let market = v.get("secMarket").and_then(Value::as_i64).and_then(market_of_sec).or_else(|| {
        account.and_then(|a| match a.market {
            TrdMarket::Us => Some(Market::Us),
            TrdMarket::Hk => Some(Market::Hk),
            _ => None,
        })
    })?;
    Some(format!("{}:{code}", market.exchange()))
}

fn id_of(v: &Value, key: &str) -> Option<String> {
    let x = v.get(key)?;
    x.as_str().map(str::to_string).or_else(|| x.as_u64().map(|n| n.to_string()))
}

pub fn position_of(p: &Value, account: Option<&AccountId>) -> Option<Value> {
    let qty = f64_of(p, "qty")?.abs();
    let short = p.get("positionSide").and_then(Value::as_i64) == Some(1);
    Some(json!({
        "id": id_of(p, "positionID")?, "ticker": ticker_of(p, account)?, "side": if short { "sell" } else { "buy" },
        "qty": qty, "avgPrice": f64_of(p, "averageCostPrice").or_else(|| f64_of(p, "costPrice")).unwrap_or(0.0),
        "last": f64_of(p, "price"), "unrealizedPnl": f64_of(p, "plVal"), "takeProfit": null, "stopLoss": null,
    }))
}

pub fn order_of(o: &Value, account: Option<&AccountId>) -> Option<Value> {
    let order_type = o.get("orderType").and_then(Value::as_i64).unwrap_or(TYPE_LIMIT as i64);
    let kind = type_of(order_type);
    let price = f64_of(o, "price");
    let aux = f64_of(o, "auxPrice");
    let message = o.get("lastErrMsg").and_then(Value::as_str).filter(|m| !m.is_empty());
    Some(json!({
        "id": id_of(o, "orderID")?, "ticker": ticker_of(o, account)?,
        "side": side_of(o.get("trdSide").and_then(Value::as_i64).unwrap_or(1)), "type": kind,
        "qty": f64_of(o, "qty").unwrap_or(0.0), "filledQty": f64_of(o, "fillQty").unwrap_or(0.0),
        "limitPrice": if matches!(kind, "limit" | "stopLimit") { price } else { None },
        "stopPrice": if matches!(kind, "stop" | "stopLimit") { aux } else { None },
        "takeProfit": null, "stopLoss": null,
        "avgFillPrice": f64_of(o, "fillAvgPrice").filter(|p| *p > 0.0),
        "status": status_of(o.get("orderStatus").and_then(Value::as_i64).unwrap_or(-1)),
        "duration": if o.get("timeInForce").and_then(Value::as_i64) == Some(TIF_GTC as i64) { "gtc" } else { "day" },
        "message": message,
        "updatedMs": f64_of(o, "updateTimestamp").map(|t| (t * 1000.0).round()),
    }))
}

pub fn execution_of(f: &Value, account: Option<&AccountId>) -> Option<Value> {
    Some(json!({
        "id": id_of(f, "fillID")?, "orderId": id_of(f, "orderID"), "ticker": ticker_of(f, account)?,
        "side": side_of(f.get("trdSide").and_then(Value::as_i64).unwrap_or(1)),
        "qty": f64_of(f, "qty").unwrap_or(0.0), "price": f64_of(f, "price").unwrap_or(0.0),
        "timeMs": f64_of(f, "createTimestamp").map_or(0.0, |t| (t * 1000.0).round()),
    }))
}

/// What OpenD said of one account, as the app's `broker.state`.
pub struct Snapshot<'a> {
    pub connected: bool,
    pub accounts: &'a [AccountId],
    pub selected: Option<AccountId>,
    pub funds: Option<Value>,
    pub positions: Vec<Value>,
    pub orders: Vec<Value>,
    pub fills: Vec<Value>,
}

pub fn state_of(s: &Snapshot) -> Value {
    let account = s.selected.as_ref();
    let accounts: Vec<Value> = s.accounts.iter().map(|a| json!({ "id": a.to_string(), "name": a.name() })).collect();
    let account_json = account.map(|a| {
        let f = s.funds.clone().unwrap_or(Value::Null);
        json!({
            "id": a.to_string(), "name": a.name(), "currency": a.market.currency(), "isDemo": !a.real,
            "balance": f64_of(&f, "cash"), "equity": f64_of(&f, "totalAssets"),
            "unrealizedPnl": f64_of(&f, "unrealizedPL"), "realizedPnl": f64_of(&f, "realizedPL"),
            "marginUsed": f64_of(&f, "initialMargin"), "marginFree": f64_of(&f, "power"),
            "values": f64_of(&f, "marketVal").map_or(vec![], |v| vec![json!({ "label": "Market value", "value": format!("{v:.2}") })]),
        })
    });
    json!({
        "connected": s.connected,
        "accounts": accounts,
        "account": account_json,
        "positionMode": "netting",
        "capabilities": { "orderTypes": ["market", "limit", "stop", "stopLimit"], "brackets": false,
                          "positionBrackets": false, "partialClose": true, "modifyOrders": true },
        // Closed positions stay in OpenD's list with no quantity.
        "positions": s.positions.iter().filter_map(|p| position_of(p, account)).filter(|p| p["qty"].as_f64() > Some(0.0)).collect::<Vec<_>>(),
        "orders": s.orders.iter().filter_map(|o| order_of(o, account)).collect::<Vec<_>>(),
        "executions": s.fills.iter().filter_map(|f| execution_of(f, account)).collect::<Vec<_>>(),
    })
}

/// The toast of an order push (Trd_UpdateOrder), when its status is one worth telling.
pub fn order_event(o: &Value) -> Option<Value> {
    let order = order_of(o, None)?;
    let status = o.get("orderStatus").and_then(Value::as_i64)?;
    let kind = match status {
        5 => "placed",
        11 => "executed",
        14 | 15 | 23 => "cancelled",
        3 | 4 | 21 | 22 => "rejected",
        _ => return None,
    };
    let price = if kind == "executed" { order["avgFillPrice"].as_f64() } else { order["limitPrice"].as_f64().or(order["stopPrice"].as_f64()) };
    Some(json!({
        "kind": kind, "ticker": order["ticker"], "side": order["side"], "type": order["type"],
        "qty": order["qty"], "price": price, "message": order["message"],
    }))
}

/// The toast of a fill push (Trd_UpdateOrderFill).
pub fn fill_event(f: &Value) -> Option<Value> {
    let e = execution_of(f, None)?;
    Some(json!({ "kind": "executed", "ticker": e["ticker"], "side": e["side"], "type": "market", "qty": e["qty"], "price": e["price"] }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sim_us() -> AccountId {
        AccountId { real: false, acc_id: 1, market: TrdMarket::Us }
    }

    fn ticket(v: Value) -> Ticket {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn a_paper_account_with_two_markets_is_two_demo_accounts() {
        let s2c = json!({"accList":[{"trdEnv":1,"accID":"9","trdMarketAuthList":[1]},{"trdEnv":0,"accID":"1","trdMarketAuthList":[2,1]}]});
        let accounts = accounts_of(&s2c);
        let ids: Vec<String> = accounts.iter().map(|a| a.to_string()).collect();
        assert_eq!(ids, ["SIM-1-US", "SIM-1-HK", "REAL-9-HK"]);
        let state = state_of(&Snapshot { connected: true, accounts: &accounts, selected: Some(accounts[1]), funds: None,
            positions: vec![], orders: vec![], fills: vec![] });
        assert_eq!((state["account"]["isDemo"].as_bool(), state["account"]["currency"].as_str()), (Some(true), Some("HKD")));
        assert_eq!(AccountId::parse("SIM-1-US"), Some(sim_us()));
        assert_eq!(AccountId::parse("SIM-1-US-x"), None);
    }

    #[test]
    fn a_gtc_stop_limit_becomes_opends_order() {
        let t = ticket(json!({"ticker":"US:AAPL","side":"buy","type":"stopLimit","qty":10,"limitPrice":101,"stopPrice":100,"duration":"gtc"}));
        let c2s = order_request(&sim_us(), &t, json!({"connID":"7","serialNo":3})).unwrap();
        assert_eq!((c2s["trdSide"].as_i64(), c2s["orderType"].as_i64(), c2s["code"].as_str(), c2s["secMarket"].as_i64()), (Some(1), Some(11), Some("AAPL"), Some(2)));
        assert_eq!((c2s["qty"].as_f64(), c2s["price"].as_f64(), c2s["auxPrice"].as_f64(), c2s["timeInForce"].as_i64()), (Some(10.0), Some(101.0), Some(100.0), Some(1)));
        assert_eq!(c2s["header"], json!({"trdEnv":0,"accID":"1","trdMarket":2}));
        assert_eq!(c2s["packetID"]["serialNo"], 3);
    }

    #[test]
    fn a_wrong_market_or_a_bracket_is_refused() {
        let hk = ticket(json!({"ticker":"HK:00700","side":"buy","type":"market","qty":100}));
        assert!(order_request(&sim_us(), &hk, json!({})).unwrap_err().contains("does not trade"));
        let tp = ticket(json!({"ticker":"US:AAPL","side":"buy","type":"market","qty":1,"takeProfit":200}));
        assert!(order_request(&sim_us(), &tp, json!({})).unwrap_err().contains("take profit"));
        let no_price = ticket(json!({"ticker":"US:AAPL","side":"buy","type":"limit","qty":1}));
        assert!(order_request(&sim_us(), &no_price, json!({})).is_err());
        let sh = ticket(json!({"ticker":"SH:600519","side":"buy","type":"limit","qty":100,"limitPrice":1500}));
        let hkcc = AccountId { real: true, acc_id: 2, market: TrdMarket::Hkcc };
        assert_eq!(order_request(&hkcc, &sh, json!({})).unwrap()["secMarket"], 31);
    }

    #[test]
    fn statuses_map_to_the_apps() {
        let got: Vec<&str> = [2, 5, 10, 11, 15, 23, 21, 22].iter().map(|s| status_of(*s)).collect();
        assert_eq!(got, ["placing", "working", "working", "filled", "cancelled", "cancelled", "rejected", "rejected"]);
    }

    #[test]
    fn closing_four_of_a_long_ten_sells_four_at_market() {
        let p = json!({"positionID":"55","positionSide":0,"code":"AAPL","secMarket":2,"qty":10,"costPrice":150,"price":160,"plVal":100});
        let t = closing_ticket(&p, Some(4.0)).unwrap();
        assert_eq!((t.side.as_str(), t.order_type.as_str(), t.qty, t.ticker.as_str()), ("sell", "market", 4.0, "US:AAPL"));
        assert!(closing_ticket(&p, Some(11.0)).is_err());
        assert_eq!(closing_ticket(&p, None).unwrap().qty, 10.0);
    }

    #[test]
    fn orders_and_fills_read_back_and_tell_their_toast() {
        let o = json!({"trdSide":2,"orderType":10,"orderStatus":11,"orderID":"88","code":"00700","secMarket":1,"qty":100,
            "price":0,"auxPrice":300,"fillQty":100,"fillAvgPrice":299.8,"timeInForce":0,"updateTimestamp":1746799230.5});
        let order = order_of(&o, None).unwrap();
        assert_eq!((order["ticker"].as_str(), order["type"].as_str(), order["stopPrice"].as_f64(), order["status"].as_str()), (Some("HK:00700"), Some("stop"), Some(300.0), Some("filled")));
        assert_eq!(order["updatedMs"].as_f64(), Some(1_746_799_230_500.0));
        let e = order_event(&o).unwrap();
        assert_eq!((e["kind"].as_str(), e["price"].as_f64()), (Some("executed"), Some(299.8)));
        let f = json!({"fillID":"4","orderID":"88","code":"AAPL","secMarket":2,"qty":5,"price":10.5,"createTimestamp":1.0,"trdSide":1});
        assert_eq!(fill_event(&f).unwrap()["qty"].as_f64(), Some(5.0));
    }
}
