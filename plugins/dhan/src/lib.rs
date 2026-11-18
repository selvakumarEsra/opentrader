/*
 * The Dhan plugin: Indian stocks, indices, futures and options from the user's Dhan account (DhanHQ API v2), as an
 * OpenTrader data plugin (plugins/kit's `feed` does the app's side):
 *   auth.rs         the day's access token from the settings' DHAN_* credentials (PIN and TOTP), or the one given
 *   instruments.rs  the scrip master, for search and symbol info
 *   history.rs      daily and intraday charts, for the bar store
 *   feed.rs         the live market feed, for live prices and streamed 1-minute bars
 */
pub mod auth;
pub mod feed;
pub mod history;
pub mod instruments;

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

/// A Dhan access token made from the PIN and TOTP, as kept on disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Token {
    access_token: String,
    client_id: String,
    ends: i64,
}

impl opentrader_plugin_kit::token::Token for Token {
    fn access(&self) -> &str {
        &self.access_token
    }
}

pub struct Dhan {
    pub urls: Urls,
    pub tokens: Tokens<Credentials, Token>,
    pace: Pace,
    agent: ureq::Agent,
}

impl Default for Dhan {
    fn default() -> Self {
        Self::new(Urls::default())
    }
}

impl Dhan {
    pub fn new(urls: Urls) -> Self {
        Dhan {
            urls,
            tokens: Tokens::new("Dhan"),
            // Dhan's data APIs: 5 requests a second.
            pace: Pace::new(4, Duration::from_secs(1)),
            agent: ureq::Agent::config_builder().http_status_as_error(false).timeout_global(Some(Duration::from_secs(60))).build().into(),
        }
    }

    pub fn client_id(&self) -> String {
        self.tokens.creds().client_id
    }

    pub async fn set_credentials(&self, c: Credentials) {
        self.tokens.set(c).await;
    }

    /// The access token: the one given in the settings, else the day's one in memory or on disk, else a new one.
    pub async fn token(&self) -> Result<String, String> {
        let c = self.tokens.creds();
        if let Some(name) = c.missing() {
            return Err(format!("Set {name} in Settings > Plugins > Dhan"));
        }
        if !c.access_token.is_empty() {
            return Ok(c.access_token);
        }
        let client = c.client_id.clone();
        // A token is used until 10 minutes before it ends.
        let usable = move |t: &Token| t.client_id == client && t.ends - 600 > now();
        let urls = self.urls.clone();
        self.tokens
            .get(usable, move || {
                let (access_token, ends) = auth::generate(&c, &urls, now())?;
                println!("[info] made Dhan's access token for {}", c.client_id);
                Ok(Token { access_token, client_id: c.client_id, ends })
            })
            .await
    }

    pub async fn invalidate(&self, access_token: &str) {
        self.tokens.invalidate(access_token).await;
    }

    /// POST `body` to `path` of the API with the token, making a new token once when Dhan refuses it.
    pub async fn post(&self, path: &str, body: Value) -> Result<Value, String> {
        for attempt in 0..2 {
            let token = self.token().await?;
            let (url, client, agent, body) = (format!("{}{path}", self.urls.api), self.client_id(), self.agent.clone(), body.clone());
            let auth = token.clone();
            let (status, text) = tokio::task::spawn_blocking(move || -> Result<(u16, String), String> {
                let resp = agent
                    .post(&url)
                    .header("access-token", &auth)
                    .header("client-id", &client)
                    .header("Accept", "application/json")
                    .header("Content-Type", "application/json")
                    .send(body.to_string())
                    .map_err(|e| e.to_string())?;
                let status = resp.status().as_u16();
                Ok((status, resp.into_body().with_config().limit(100 * 1024 * 1024).read_to_string().map_err(|e| e.to_string())?))
            })
            .await
            .map_err(|e| e.to_string())??;
            let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
            let refused = status == 401 || v["errorCode"] == "DH-901";
            if refused && attempt == 0 && self.tokens.creds().access_token.is_empty() {
                self.invalidate(&token).await;
                continue;
            }
            if status == 200 || v["errorMessage"].is_string() {
                return Ok(v);
            }
            return Err(format!("Dhan: HTTP {status}"));
        }
        Err("Dhan refused the access token".into())
    }
}

impl Source for Dhan {
    async fn fetch(&self, inst: &Instrument, resolution: &str, from: i64, to: i64) -> Result<Vec<Bar>, String> {
        let (path, body) = history::request(inst, resolution, from, to);
        self.pace.wait().await;
        let v = self.post(path, body).await?;
        let mut bars = history::candles(&v, matches!(resolution, "1D" | "D"))?;
        bars.retain(|b| (from as f64..=to as f64).contains(&b[0]));
        Ok(bars)
    }

    fn step(&self, resolution: &str) -> Option<i64> {
        history::step_days(resolution).map(|d| d * 86_400)
    }

    fn oldest(&self, resolution: &str, now: i64) -> Option<i64> {
        (!matches!(resolution, "1D" | "D")).then(|| now - history::INTRADAY_YEARS * 365 * 86_400)
    }
}

impl Provider for Dhan {
    const ID: &'static str = "dhan";
    const INTERVALS: &'static [&'static str] = history::INTERVALS;
    const CURRENCY: &'static str = "INR";

    fn calendar(exchange: &str) -> Calendar {
        Calendar::india(exchange)
    }

    async fn configure(&self, settings: &HashMap<String, String>) {
        self.set_credentials(Credentials::from_settings(settings)).await;
    }

    async fn instruments(&self) -> Result<Vec<Instrument>, String> {
        let (url, agent) = (self.urls.scrips.clone(), self.agent.clone());
        let text = tokio::task::spawn_blocking(move || -> Result<String, String> {
            let resp = agent.get(&url).call().map_err(|e| format!("the scrip master: {e}"))?;
            if resp.status().as_u16() != 200 {
                return Err(format!("the scrip master: HTTP {}", resp.status().as_u16()));
            }
            resp.into_body().with_config().limit(300 * 1024 * 1024).read_to_string().map_err(|e| format!("the scrip master: {e}"))
        })
        .await
        .map_err(|e| e.to_string())??;
        let today = chrono::DateTime::from_timestamp(now() + 19_800, 0).map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default();
        tokio::task::spawn_blocking(move || instruments::parse(&text, &today)).await.map_err(|e| e.to_string())?
    }

    async fn stream(self: Arc<Self>, wanted: watch::Receiver<Vec<Instrument>>, events: mpsc::UnboundedSender<Event>) {
        opentrader_plugin_kit::socket::run(self, wanted, events).await
    }
}
