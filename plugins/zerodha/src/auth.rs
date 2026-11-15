/*
 * The Kite Connect login without a browser: the user's Kite user id, password and a TOTP code (from the secret shown
 * when TOTP was turned on) log in to Kite as the login page would, which hands the Kite Connect app's
 * `request_token` to its redirect URL; that token and the API secret's checksum make the day's `access_token`
 * (POST /session/token). Kite access tokens end at 06:00 IST the next morning.
 *
 * The user must have authorised their Kite Connect app once in a browser (the first login asks for it).
 */
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::time::Duration;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Credentials {
    pub api_key: String,
    pub api_secret: String,
    pub user_id: String,
    pub password: String,
    pub totp_secret: String,
}

impl Credentials {
    /// From the plugin's settings, else the environment (`KITE_API_KEY`…).
    pub fn from_settings(settings: &HashMap<String, String>) -> Credentials {
        let get = |key: &str, env: &str| {
            settings.get(key).map(|v| v.trim().to_string()).filter(|v| !v.is_empty()).or_else(|| std::env::var(env).ok().map(|v| v.trim().to_string())).unwrap_or_default()
        };
        Credentials {
            api_key: get("apiKey", "KITE_API_KEY"),
            api_secret: get("apiSecret", "KITE_API_SECRET"),
            user_id: get("userId", "KITE_USER_ID"),
            password: get("password", "KITE_PASSWORD"),
            totp_secret: get("totpSecret", "KITE_TOTP_SECRET"),
        }
    }

    /// The first credential not given, by its name.
    pub fn missing(&self) -> Option<&'static str> {
        [
            (&self.api_key, "KITE_API_KEY"),
            (&self.api_secret, "KITE_API_SECRET"),
            (&self.user_id, "KITE_USER_ID"),
            (&self.password, "KITE_PASSWORD"),
            (&self.totp_secret, "KITE_TOTP_SECRET"),
        ]
        .into_iter()
        .find(|(v, _)| v.is_empty())
        .map(|(_, name)| name)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Urls {
    /// The login site, `https://kite.zerodha.com`.
    pub login: String,
    /// The API, `https://api.kite.trade`.
    pub api: String,
    /// The stream, `wss://ws.kite.trade`.
    pub ws: String,
}

impl Default for Urls {
    fn default() -> Self {
        let env = |k: &str, d: &str| std::env::var(k).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| d.to_string());
        Urls {
            login: env("OPENTRADER_ZERODHA_LOGIN_URL", "https://kite.zerodha.com"),
            api: env("OPENTRADER_ZERODHA_API_URL", "https://api.kite.trade"),
            ws: env("OPENTRADER_ZERODHA_WS_URL", "wss://ws.kite.trade"),
        }
    }
}

/// The /session/token checksum: SHA-256 (hex) of api_key + request_token + api_secret.
pub fn checksum(api_key: &str, request_token: &str, api_secret: &str) -> String {
    let digest = Sha256::digest(format!("{api_key}{request_token}{api_secret}").as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// The `request_token` of a redirect's URL, if it carries one.
pub fn request_token_of(url: &str) -> Option<String> {
    let query = url.split_once('?')?.1;
    query.split('&').find_map(|kv| kv.strip_prefix("request_token=")).map(|t| t.split('#').next().unwrap_or(t).to_string()).filter(|t| !t.is_empty())
}

fn query_param(url: &str, name: &str) -> Option<String> {
    let query = url.split_once('?')?.1;
    query.split('&').find_map(|kv| kv.strip_prefix(&format!("{name}="))).map(str::to_string)
}

fn absolute(base: &str, location: &str) -> String {
    if location.starts_with("http://") || location.starts_with("https://") {
        location.to_string()
    } else {
        format!("{}{}", base.trim_end_matches('/'), if location.starts_with('/') { location.to_string() } else { format!("/{location}") })
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .max_redirects(0)
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(30)))
        .user_agent("Mozilla/5.0 (OpenTrader Kite Connect plugin)")
        .build()
        .into()
}

enum Hop {
    Token(String),
    Page(String),
}

/// Follows redirects from `url` on the login site: the request_token when a redirect carries it, else the last page.
fn follow(agent: &ureq::Agent, base: &str, url: String) -> Result<Hop, String> {
    let mut url = url;
    for _ in 0..10 {
        if let Some(t) = request_token_of(&url) {
            return Ok(Hop::Token(t));
        }
        let resp = agent.get(&url).call().map_err(|e| format!("Kite login: {e}"))?;
        let status = resp.status().as_u16();
        let location = resp.headers().get("location").and_then(|v| v.to_str().ok()).map(str::to_string);
        match location {
            Some(loc) if (300..400).contains(&status) => {
                if let Some(t) = request_token_of(&loc) {
                    return Ok(Hop::Token(t));
                }
                url = absolute(base, &loc);
            }
            _ => return Ok(Hop::Page(url)),
        }
    }
    Err("Kite login: too many redirects".into())
}

fn json_of(resp: ureq::http::Response<ureq::Body>, step: &str) -> Result<Value, String> {
    let status = resp.status().as_u16();
    let text = resp.into_body().read_to_string().map_err(|e| format!("Kite {step}: {e}"))?;
    let v: Value = serde_json::from_str(&text).map_err(|_| format!("Kite {step}: HTTP {status}, not JSON"))?;
    if v["status"] != "success" {
        let message = v["message"].as_str().unwrap_or("refused");
        return Err(format!("Kite {step}: {message}"));
    }
    Ok(v)
}

/// Logs in and makes the day's access token (blocking: run it off the async threads).
pub fn login(c: &Credentials, urls: &Urls, now: i64) -> Result<String, String> {
    if let Some(name) = c.missing() {
        return Err(format!("Set {name} in Settings > Plugins > Zerodha"));
    }
    let agent = agent();
    let start = format!("{}/connect/login?v=3&api_key={}", urls.login, c.api_key);
    let sess_id = match follow(&agent, &urls.login, start.clone())? {
        Hop::Token(t) => return session(&agent, c, urls, &t),
        Hop::Page(page) => query_param(&page, "sess_id"),
    };
    let r = agent
        .post(format!("{}/api/login", urls.login))
        .send_form([("user_id", c.user_id.as_str()), ("password", c.password.as_str())])
        .map_err(|e| format!("Kite login: {e}"))?;
    let v = json_of(r, "login")?;
    let request_id = v["data"]["request_id"].as_str().ok_or("Kite login: no request id")?.to_string();
    let code = opentrader_plugin_kit::totp::code(&c.totp_secret, now)?;
    let r = agent
        .post(format!("{}/api/twofa", urls.login))
        .send_form([
            ("user_id", c.user_id.as_str()),
            ("request_id", request_id.as_str()),
            ("twofa_value", code.as_str()),
            ("twofa_type", "totp"),
            ("skip_totp", "true"),
        ])
        .map_err(|e| format!("Kite TOTP: {e}"))?;
    json_of(r, "TOTP")?;
    let finish = match sess_id {
        Some(s) => format!("{}/connect/login?api_key={}&sess_id={s}", urls.login, c.api_key),
        None => start,
    };
    match follow(&agent, &urls.login, finish)? {
        Hop::Token(t) => session(&agent, c, urls, &t),
        Hop::Page(page) if page.contains("authorize") => Err(format!(
            "Kite: authorise the Kite Connect app once in a browser ({}/connect/login?v=3&api_key={}), then the plugin logs in on its own",
            urls.login, c.api_key
        )),
        Hop::Page(_) => Err("Kite login: no request token (is the Kite Connect app's redirect URL set?)".into()),
    }
}

/// POST /session/token: the access token of `request_token`.
fn session(agent: &ureq::Agent, c: &Credentials, urls: &Urls, request_token: &str) -> Result<String, String> {
    let sum = checksum(&c.api_key, request_token, &c.api_secret);
    let r = agent
        .post(format!("{}/session/token", urls.api))
        .header("X-Kite-Version", "3")
        .send_form([("api_key", c.api_key.as_str()), ("request_token", request_token), ("checksum", sum.as_str())])
        .map_err(|e| format!("Kite session: {e}"))?;
    let v = json_of(r, "session")?;
    v["data"]["access_token"].as_str().map(str::to_string).ok_or_else(|| "Kite session: no access token".into())
}

/// The start of the current Kite token day: the last 06:00 IST at or before `now`.
pub fn token_day(now: i64) -> i64 {
    const IST: i64 = 19_800;
    let local = now + IST;
    let six = local - local.rem_euclid(86_400) + 6 * 3600;
    (if local >= six { six } else { six - 86_400 }) - IST
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_session_checksum_is_sha256_of_key_token_secret() {
        // sha256("abcdefghijk").
        assert_eq!(checksum("abc", "defg", "hijk"), "ca2f2069ea0c6e4658222e06f8dd639659cbb5e67cbbba6734bc334a3799bc68");
    }

    #[test]
    fn request_tokens_are_read_from_redirects() {
        assert_eq!(request_token_of("https://example.com/cb?action=login&type=login&status=success&request_token=Ab12Cd"), Some("Ab12Cd".into()));
        assert_eq!(request_token_of("https://kite.zerodha.com/connect/login?api_key=x&sess_id=y"), None);
    }

    #[test]
    fn missing_credentials_are_named() {
        let mut c = Credentials { api_key: "k".into(), api_secret: "s".into(), user_id: "AB1234".into(), password: "p".into(), totp_secret: String::new() };
        assert_eq!(c.missing(), Some("KITE_TOTP_SECRET"));
        c.totp_secret = "GEZDGNBV".into();
        assert_eq!(c.missing(), None);
        let err = login(&Credentials::default(), &Urls::default(), 0).unwrap_err();
        assert_eq!(err, "Set KITE_API_KEY in Settings > Plugins > Zerodha");
    }

    #[test]
    fn a_token_day_starts_at_six_in_the_morning_ist() {
        // 2026-11-16 05:59 IST → the day began 2026-11-15 06:00 IST; 06:00 → that morning.
        let six_am_ist = 1_794_789_000; // 2026-11-16 06:00 IST
        assert_eq!(token_day(six_am_ist - 60), six_am_ist - 86_400);
        assert_eq!(token_day(six_am_ist), six_am_ist);
        assert_eq!(token_day(six_am_ist + 3600 * 20), six_am_ist);
    }
}
