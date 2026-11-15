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

/// A refused login is not tried again sooner.
const LOGIN_RETRY_SECS: i64 = 60;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Token {
    access_token: String,
    api_key: String,
    user_id: String,
    made: i64,
}

pub struct Zerodha {
    pub urls: Urls,
    creds: RwLock<Credentials>,
    token: tokio::sync::Mutex<Option<Token>>,
    /// The last refused login: when, and why.
    failed: Mutex<Option<(i64, String)>>,
    pace: Pace,
    agent: ureq::Agent,
    /// Told when the settings change (the stream tries again at once).
    pub configured: Notify,
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn token_file() -> Option<PathBuf> {
    opentrader_plugin_kit::store::cache_dir().map(|d| d.join("token.json"))
}

/// Writes `body` readable by the user only.
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

impl Default for Zerodha {
    fn default() -> Self {
        Self::new(Urls::default())
    }
}

impl Zerodha {
    pub fn new(urls: Urls) -> Self {
        Zerodha {
            urls,
            creds: RwLock::new(Credentials::default()),
            token: tokio::sync::Mutex::new(None),
            failed: Mutex::new(None),
            // Kite's history limit: 3 requests a second.
            pace: Pace::new(3, Duration::from_secs(1)),
            agent: ureq::Agent::config_builder().http_status_as_error(false).timeout_global(Some(Duration::from_secs(60))).build().into(),
            configured: Notify::new(),
        }
    }

    fn creds(&self) -> Credentials {
        self.creds.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn api_key(&self) -> String {
        self.creds().api_key
    }

    /// Takes new credentials; a changed login forgets the token and the last refusal.
    pub async fn set_credentials(&self, c: Credentials) {
        let changed = { *self.creds.read().unwrap_or_else(|e| e.into_inner()) != c };
        if changed {
            *self.creds.write().unwrap_or_else(|e| e.into_inner()) = c;
            *self.token.lock().await = None;
            *self.failed.lock().unwrap_or_else(|e| e.into_inner()) = None;
            self.configured.notify_waiters();
        }
    }

    /// The day's access token: the one in memory or on disk when it is of today's Kite day, else a new login.
    pub async fn token(&self) -> Result<String, String> {
        let c = self.creds();
        if let Some(name) = c.missing() {
            return Err(format!("Set {name} in Settings > Plugins > Zerodha"));
        }
        let mut held = self.token.lock().await;
        let day = auth::token_day(now());
        let usable = |t: &Token| t.api_key == c.api_key && t.user_id == c.user_id && t.made >= day;
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
        let urls = self.urls.clone();
        let made = now();
        let login = { c.clone() };
        let result = tokio::task::spawn_blocking(move || auth::login(&login, &urls, made)).await.map_err(|e| e.to_string())?;
        match result {
            Ok(access_token) => {
                println!("[info] logged in to Kite as {}", c.user_id);
                let t = Token { access_token: access_token.clone(), api_key: c.api_key, user_id: c.user_id, made };
                if let Some(f) = token_file() {
                    if let Err(e) = write_private(&f, &serde_json::to_vec(&t).unwrap_or_default()) {
                        println!("[warn] the Kite token could not be kept on disk: {e}");
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

    /// The broker refused `access_token`: the next call logs in again.
    pub async fn invalidate(&self, access_token: &str) {
        let mut held = self.token.lock().await;
        if held.as_ref().is_some_and(|t| t.access_token == access_token) {
            *held = None;
        }
        if let Some(f) = token_file() {
            let _ = std::fs::remove_file(f);
        }
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

    async fn configure(&self, settings: &HashMap<String, String>) {
        self.set_credentials(Credentials::from_settings(settings)).await;
    }

    async fn instruments(&self) -> Result<Vec<Instrument>, String> {
        let body = self.get("/instruments").await?;
        let text = instruments::text_of(body)?;
        tokio::task::spawn_blocking(move || instruments::parse(&text)).await.map_err(|e| e.to_string())?
    }

    async fn stream(self: Arc<Self>, wanted: watch::Receiver<Vec<Instrument>>, events: mpsc::UnboundedSender<Event>) {
        ticker::run(self, wanted, events).await
    }
}
