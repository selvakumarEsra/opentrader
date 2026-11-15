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
use opentrader_plugin_kit::catalog::Instrument;
use opentrader_plugin_kit::feed::{Event, Provider};
use opentrader_plugin_kit::history::Source;
use opentrader_plugin_kit::pace::Pace;
use opentrader_plugin_kit::Bar;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use tokio::sync::{mpsc, watch, Notify};

/// A refused token request is not tried again sooner.
const LOGIN_RETRY_SECS: i64 = 60;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Token {
    access_token: String,
    client_id: String,
    ends: i64,
}

pub struct Dhan {
    pub urls: Urls,
    creds: RwLock<Credentials>,
    token: tokio::sync::Mutex<Option<Token>>,
    failed: Mutex<Option<(i64, String)>>,
    pace: Pace,
    agent: ureq::Agent,
    /// Told when the settings change (the feed tries again at once).
    pub configured: Notify,
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn token_file() -> Option<PathBuf> {
    opentrader_plugin_kit::store::cache_dir().map(|d| d.join("token.json"))
}

fn write_private(path: &PathBuf, body: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, body)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, path)
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
            creds: RwLock::new(Credentials::default()),
            token: tokio::sync::Mutex::new(None),
            failed: Mutex::new(None),
            // Dhan's data APIs: 5 requests a second.
            pace: Pace::new(4, Duration::from_secs(1)),
            agent: ureq::Agent::config_builder().http_status_as_error(false).timeout_global(Some(Duration::from_secs(60))).build().into(),
            configured: Notify::new(),
        }
    }

    fn creds(&self) -> Credentials {
        self.creds.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn client_id(&self) -> String {
        self.creds().client_id
    }

    pub async fn set_credentials(&self, c: Credentials) {
        let changed = { *self.creds.read().unwrap_or_else(|e| e.into_inner()) != c };
        if changed {
            *self.creds.write().unwrap_or_else(|e| e.into_inner()) = c;
            *self.token.lock().await = None;
            *self.failed.lock().unwrap_or_else(|e| e.into_inner()) = None;
            self.configured.notify_waiters();
        }
    }

    /// The access token: the one given in the settings, else the day's one in memory or on disk, else a new one.
    pub async fn token(&self) -> Result<String, String> {
        let c = self.creds();
        if let Some(name) = c.missing() {
            return Err(format!("Set {name} in Settings > Plugins > Dhan"));
        }
        if !c.access_token.is_empty() {
            return Ok(c.access_token);
        }
        let mut held = self.token.lock().await;
        // A token is used until 10 minutes before it ends.
        let usable = |t: &Token| t.client_id == c.client_id && t.ends - 600 > now();
        if let Some(t) = held.as_ref().filter(|t| usable(t)) {
            return Ok(t.access_token.clone());
        }
        if let Some(t) = token_file().and_then(|f| std::fs::read(f).ok()).and_then(|b| serde_json::from_slice::<Token>(&b).ok()).filter(usable) {
            *held = Some(t.clone());
            return Ok(t.access_token);
        }
        if let Some((at, why)) = self.failed.lock().unwrap_or_else(|e| e.into_inner()).clone() {
            if now() - at < LOGIN_RETRY_SECS {
                return Err(why);
            }
        }
        let (urls, made, login) = (self.urls.clone(), now(), c.clone());
        match tokio::task::spawn_blocking(move || auth::generate(&login, &urls, made)).await.map_err(|e| e.to_string())? {
            Ok((access_token, ends)) => {
                println!("[info] made Dhan's access token for {}", c.client_id);
                let t = Token { access_token: access_token.clone(), client_id: c.client_id, ends };
                if let Some(f) = token_file() {
                    if let Err(e) = write_private(&f, &serde_json::to_vec(&t).unwrap_or_default()) {
                        println!("[warn] the Dhan token could not be kept on disk: {e}");
                    }
                }
                *held = Some(t);
                Ok(access_token)
            }
            Err(e) => {
                *self.failed.lock().unwrap_or_else(|e| e.into_inner()) = Some((now(), e.clone()));
                Err(e)
            }
        }
    }

    pub async fn invalidate(&self, access_token: &str) {
        let mut held = self.token.lock().await;
        if held.as_ref().is_some_and(|t| t.access_token == access_token) {
            *held = None;
        }
        if let Some(f) = token_file() {
            let _ = std::fs::remove_file(f);
        }
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
            if refused && attempt == 0 && self.creds().access_token.is_empty() {
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
        feed::run(self, wanted, events).await
    }
}
