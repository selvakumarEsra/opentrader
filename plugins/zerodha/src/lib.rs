/*
 * The Zerodha plugin: Indian stocks, indices, futures and options from the user's Kite Connect app, as an OpenTrader
 * data plugin (plugins/kit's `feed` does the app's side):
 *   auth.rs         the login from the settings' KITE_* credentials (user id, password, TOTP), the day's token
 *   instruments.rs  the instrument dump, for search and symbol info
 *   history.rs      historical candles, for the bar store
 *   ticker.rs       the WebSocket stream, for live prices and streamed 1-minute bars
 */
pub mod auth;
pub mod history;
pub mod instruments;
pub mod ticker;

use auth::{Credentials, Urls};
use opentrader_plugin_kit::calendar::Calendar;
use opentrader_plugin_kit::catalog::Instrument;
use opentrader_plugin_kit::feed::{Event, Provider};
use opentrader_plugin_kit::history::Source;
use opentrader_plugin_kit::pace::Pace;
use opentrader_plugin_kit::token::Tokens;
use opentrader_plugin_kit::{now, Bar};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, watch};

/// The day's Kite token, as kept on disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Token {
    access_token: String,
    api_key: String,
    user_id: String,
    made: i64,
}

impl opentrader_plugin_kit::token::Token for Token {
    fn access(&self) -> &str {
        &self.access_token
    }
}

pub struct Zerodha {
    pub urls: Urls,
    pub tokens: Tokens<Credentials, Token>,
    pace: Pace,
    agent: ureq::Agent,
}

impl Default for Zerodha {
    fn default() -> Self {
        Self::new(Urls::default())
    }
}

impl Zerodha {
    pub fn new(urls: Urls) -> Self {
        Zerodha {
            urls,
            tokens: Tokens::new("Kite"),
            // Kite's history limit: 3 requests a second.
            pace: Pace::new(3, Duration::from_secs(1)),
            agent: ureq::Agent::config_builder().http_status_as_error(false).timeout_global(Some(Duration::from_secs(60))).build().into(),
        }
    }

    pub fn api_key(&self) -> String {
        self.tokens.creds().api_key
    }

    /// Takes new credentials; a changed login forgets the token and the last refusal.
    pub async fn set_credentials(&self, c: Credentials) {
        self.tokens.set(c).await;
    }

    /// The day's access token: the one in memory or on disk when it is of today's Kite day, else a new login.
    pub async fn token(&self) -> Result<String, String> {
        let c = self.tokens.creds();
        if let Some(name) = c.missing() {
            return Err(format!("Set {name} in Settings > Plugins > Zerodha"));
        }
        let day = auth::token_day(now());
        let (key, user) = (c.api_key.clone(), c.user_id.clone());
        let usable = move |t: &Token| t.api_key == key && t.user_id == user && t.made >= day;
        let urls = self.urls.clone();
        self.tokens
            .get(usable, move || {
                let made = now();
                let access_token = auth::login(&c, &urls, made)?;
                println!("[info] logged in to Kite as {}", c.user_id);
                Ok(Token { access_token, api_key: c.api_key, user_id: c.user_id, made })
            })
            .await
    }

    /// The broker refused `access_token`: the next call logs in again.
    pub async fn invalidate(&self, access_token: &str) {
        self.tokens.invalidate(access_token).await;
    }

    /// GET `path` of the API with the token: the body, logging in again once when Kite refuses the token.
    pub async fn get(&self, path: &str) -> Result<Vec<u8>, String> {
        for attempt in 0..2 {
            let token = self.token().await?;
            let url = format!("{}{path}", self.urls.api);
            let auth = format!("token {}:{token}", self.api_key());
            let agent = self.agent.clone();
            let (status, body) = tokio::task::spawn_blocking(move || -> Result<(u16, Vec<u8>), String> {
                let resp = agent.get(&url).header("X-Kite-Version", "3").header("Authorization", &auth).call().map_err(|e| e.to_string())?;
                let status = resp.status().as_u16();
                let body = resp.into_body().with_config().limit(200 * 1024 * 1024).read_to_vec().map_err(|e| e.to_string())?;
                Ok((status, body))
            })
            .await
            .map_err(|e| e.to_string())??;
            if status == 200 {
                return Ok(body);
            }
            let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            let message = v["message"].as_str().map(str::to_string).unwrap_or_else(|| format!("HTTP {status}"));
            if status == 403 && v["error_type"] == "TokenException" && attempt == 0 {
                self.invalidate(&token).await;
                continue;
            }
            return Err(format!("Kite: {message}"));
        }
        Err("Kite refused the token".into())
    }
}

impl Source for Zerodha {
    async fn fetch(&self, inst: &Instrument, resolution: &str, from: i64, to: i64) -> Result<Vec<Bar>, String> {
        let (interval, _) = history::interval_of(resolution).ok_or_else(|| format!("Resolution {resolution} is not served"))?;
        self.pace.wait().await;
        let enc = |t: i64| history::ist_text(t).replace(' ', "+");
        let path = format!("/instruments/historical/{}/{interval}?from={}&to={}&oi=0", inst.id, enc(from), enc(to));
        let body = self.get(&path).await?;
        let v: Value = serde_json::from_slice(&body).map_err(|e| format!("Kite history: {e}"))?;
        history::candles(&v)
    }

    fn step(&self, resolution: &str) -> Option<i64> {
        history::interval_of(resolution).map(|(_, days)| days * 86_400)
    }
}

impl Provider for Zerodha {
    const ID: &'static str = "zerodha";
    const INTERVALS: &'static [&'static str] = history::INTERVALS;
    const CURRENCY: &'static str = "INR";

    fn calendar(exchange: &str) -> Calendar {
        Calendar::india(exchange)
    }

    async fn configure(&self, settings: &HashMap<String, String>) {
        self.set_credentials(Credentials::from_settings(settings)).await;
    }

    async fn instruments(&self) -> Result<Vec<Instrument>, String> {
        // The instrument dump is public: no login, so search works while the credentials are wrong or missing.
        let (url, agent) = (format!("{}/instruments", self.urls.api), self.agent.clone());
        let body = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, String> {
            let resp = agent.get(&url).header("X-Kite-Version", "3").call().map_err(|e| format!("Kite instruments: {e}"))?;
            match resp.status().as_u16() {
                200 => resp.into_body().with_config().limit(200 * 1024 * 1024).read_to_vec().map_err(|e| format!("Kite instruments: {e}")),
                status => Err(format!("Kite instruments: HTTP {status}")),
            }
        })
        .await
        .map_err(|e| e.to_string())??;
        let text = instruments::text_of(body)?;
        tokio::task::spawn_blocking(move || instruments::parse(&text)).await.map_err(|e| e.to_string())?
    }

    async fn stream(self: Arc<Self>, wanted: watch::Receiver<Vec<Instrument>>, events: mpsc::UnboundedSender<Event>) {
        opentrader_plugin_kit::socket::run(self, wanted, events).await
    }
}
