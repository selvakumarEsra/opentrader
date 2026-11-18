/*
 * A broker's WebSocket stream of ticks, for the plugin's life (`run`): connected while there is something to stream
 * and a token, subscribed to the wanted instruments as they change, connected again after a drop. A connection that
 * fails, or drops soon after it was made, waits twice as long each time (2 s up to a minute); one that stayed up a
 * while starts again from 2 s. The broker's part (`Broker`) is its URL, its subscribe messages and its frames.
 */
use crate::catalog::Instrument;
use crate::feed::Event;
use crate::live::Tick;
use futures_util::{SinkExt, StreamExt};
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, watch, Notify};
use tokio_tungstenite::tungstenite::Message;

const FIRST_WAIT: Duration = Duration::from_secs(2);
const LONGEST_WAIT: Duration = Duration::from_secs(60);
/// A connection up this long was not refused: the next one after it waits the least again.
const STABLE: Duration = Duration::from_secs(30);
/// How long to wait for a token after one was refused (unless the credentials change sooner).
const TOKEN_WAIT: Duration = Duration::from_secs(60);

/// Why a frame ends the connection.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Closed {
    /// The broker said so.
    ByBroker,
    /// The token ended: the next connection makes a new one.
    TokenEnded,
}

pub trait Broker: Send + Sync + 'static {
    /// The stream's name in messages: `Kite's stream`.
    const NAME: &'static str;
    /// What the broker's frames need kept between them and across connections (previous closes).
    type State: Default + Send;

    fn token(&self) -> impl Future<Output = Result<String, String>> + Send;
    fn invalidate(&self, token: &str) -> impl Future<Output = ()> + Send;
    /// Told when the credentials change.
    fn configured(&self) -> &Notify;
    /// The stream's URL with `token`.
    fn url(&self, token: &str) -> String;
    /// A failed connection's error says the token was refused.
    fn refused(&self, _error: &str) -> bool {
        false
    }
    /// The key frames name `inst` by.
    fn key(&self, inst: &Instrument) -> String;
    /// The messages subscribing `new` and unsubscribing `gone`.
    fn subscribe(&self, new: &[&Instrument], gone: &[&Instrument]) -> Vec<String>;
    /// The messages sent before closing a connection with nothing left to stream.
    fn goodbye(&self) -> Vec<String> {
        Vec::new()
    }
    /// The ticks of a binary frame, of the instruments `held` (by key); Err ends the connection.
    fn frame(&self, state: &mut Self::State, frame: &[u8], held: &HashMap<String, Instrument>, now: i64, ticks: &mut Vec<Tick>) -> Result<(), Closed>;
    /// A text frame (errors, notices).
    fn text(&self, _text: &str) {}
}

/// How a connection ended.
enum End {
    /// Nothing is wanted any more.
    Idle,
    Dropped,
    /// The plugin is stopping.
    Stop,
}

/// The stream for the plugin's life.
pub async fn run<B: Broker>(b: Arc<B>, mut wanted: watch::Receiver<Vec<Instrument>>, events: mpsc::UnboundedSender<Event>) {
    let mut wait = FIRST_WAIT;
    let mut state = B::State::default();
    loop {
        if wanted.borrow().is_empty() {
            if wanted.changed().await.is_err() {
                return;
            }
            continue;
        }
        let token = match b.token().await {
            Ok(t) => t,
            Err(e) => {
                println!("[warn] live prices are off: {e}");
                tokio::select! {
                    _ = tokio::time::sleep(TOKEN_WAIT) => {}
                    _ = b.configured().notified() => {}
                }
                continue;
            }
        };
        let ws = match tokio_tungstenite::connect_async(b.url(&token).as_str()).await {
            Ok((ws, _)) => ws,
            Err(e) => {
                let text = e.to_string();
                if b.refused(&text) {
                    b.invalidate(&token).await;
                }
                println!("[warn] {}: {}", B::NAME, text.replace(&token, "…"));
                tokio::time::sleep(wait).await;
                wait = (wait * 2).min(LONGEST_WAIT);
                continue;
            }
        };
        println!("[info] connected to {}", B::NAME);
        let _ = events.send(Event::Connected);
        let up = tokio::time::Instant::now();
        let end = session(&*b, &token, ws, &mut wanted, &events, &mut state).await;
        println!("[info] {} closed", B::NAME);
        match end {
            End::Stop => return,
            End::Idle => continue,
            End::Dropped => {}
        }
        if up.elapsed() >= STABLE {
            wait = FIRST_WAIT;
        }
        tokio::time::sleep(wait).await;
        wait = (wait * 2).min(LONGEST_WAIT);
    }
}

async fn session<B: Broker, S>(
    b: &B,
    token: &str,
    ws: S,
    wanted: &mut watch::Receiver<Vec<Instrument>>,
    events: &mpsc::UnboundedSender<Event>,
    state: &mut B::State,
) -> End
where
    S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + futures_util::Sink<Message> + Unpin,
{
    let (mut tx, mut rx) = ws.split();
    let mut held: HashMap<String, Instrument> = HashMap::new();
    let mut ticks = Vec::new();
    loop {
        // The set to hold: the new ones subscribed, the dropped ones unsubscribed.
        let want: HashMap<String, Instrument> = wanted.borrow_and_update().iter().map(|i| (b.key(i), i.clone())).collect();
        let new: Vec<&Instrument> = want.iter().filter(|(k, _)| !held.contains_key(*k)).map(|(_, i)| i).collect();
        let gone: Vec<&Instrument> = held.iter().filter(|(k, _)| !want.contains_key(*k)).map(|(_, i)| i).collect();
        for m in b.subscribe(&new, &gone) {
            if tx.send(Message::text(m)).await.is_err() {
                return End::Dropped;
            }
        }
        held = want;
        if held.is_empty() {
            for m in b.goodbye() {
                let _ = tx.send(Message::text(m)).await;
            }
            let _ = tx.send(Message::Close(None)).await;
            return End::Idle;
        }
        loop {
            tokio::select! {
                changed = wanted.changed() => {
                    if changed.is_err() { return End::Stop; }
                    break;
                }
                msg = rx.next() => match msg {
                    Some(Ok(Message::Binary(frame))) => {
                        let read = b.frame(state, &frame, &held, crate::now(), &mut ticks);
                        for t in ticks.drain(..) {
                            let _ = events.send(Event::Tick(t));
                        }
                        match read {
                            Ok(()) => {}
                            Err(Closed::TokenEnded) => {
                                b.invalidate(token).await;
                                return End::Dropped;
                            }
                            Err(Closed::ByBroker) => return End::Dropped,
                        }
                    }
                    Some(Ok(Message::Text(text))) => b.text(&text),
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return End::Dropped,
                    Some(Ok(_)) => {}
                }
            }
        }
    }
}
