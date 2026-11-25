/*
 * Dhan's access token, without a browser: the client id, the 6-digit PIN and a TOTP code (from the secret shown when
 * TOTP was turned on) make the day's token (POST auth.dhan.co/app/generateAccessToken?dhanClientId=&pin=&totp=);
 * it lasts 24 hours. Or the user pastes a token made at web.dhan.co, used as given.
 */
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Credentials {
    pub client_id: String,
    pub pin: String,
    pub totp_secret: String,
    /// A token made at web.dhan.co, used instead of the PIN and TOTP.
    pub access_token: String,
}

impl Credentials {
    /// From the plugin's settings, else the environment (`DHAN_CLIENT_ID`…).
    pub fn from_settings(settings: &HashMap<String, String>) -> Credentials {
        let get = |key: &str, env: &str| {
            settings.get(key).map(|v| v.trim().to_string()).filter(|v| !v.is_empty()).or_else(|| std::env::var(env).ok().map(|v| v.trim().to_string())).unwrap_or_default()
        };
        Credentials {
            client_id: get("clientId", "DHAN_CLIENT_ID"),
            pin: get("pin", "DHAN_PIN"),
            totp_secret: get("totpSecret", "DHAN_TOTP_SECRET"),
            access_token: get("accessToken", "DHAN_ACCESS_TOKEN"),
        }
    }

    /// The first credential not given, by its name.
    pub fn missing(&self) -> Option<&'static str> {
        if self.client_id.is_empty() {
            return Some("DHAN_CLIENT_ID");
        }
        if !self.access_token.is_empty() {
            return None;
        }
        if self.pin.is_empty() {
            return Some("DHAN_PIN (or DHAN_ACCESS_TOKEN)");
        }
        self.totp_secret.is_empty().then_some("DHAN_TOTP_SECRET (or DHAN_ACCESS_TOKEN)")
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Urls {
    /// `https://auth.dhan.co`.
    pub auth: String,
    /// `https://api.dhan.co`.
    pub api: String,
    /// `wss://api-feed.dhan.co`.
    pub feed: String,
    /// The instrument list, `https://images.dhan.co/api-data/api-scrip-master.csv`.
    pub scrips: String,
}

impl Default for Urls {
    fn default() -> Self {
        let env = |k: &str, d: &str| std::env::var(k).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| d.to_string());
        Urls {
            auth: env("OPENTRADER_DHAN_AUTH_URL", "https://auth.dhan.co"),
            api: env("OPENTRADER_DHAN_API_URL", "https://api.dhan.co"),
            feed: env("OPENTRADER_DHAN_FEED_URL", "wss://api-feed.dhan.co"),
            scrips: env("OPENTRADER_DHAN_SCRIPS_URL", "https://images.dhan.co/api-data/api-scrip-master.csv"),
        }
    }
}

/// `expiryTime` (`2025-09-23T12:37:23` or with a zone, IST when without) as UNIX seconds.
pub fn expiry_of(text: &str) -> Option<i64> {
    if let Ok(d) = chrono::DateTime::parse_from_rfc3339(text) {
        return Some(d.timestamp());
    }
    let head = text.split('.').next()?;
    let naive = chrono::NaiveDateTime::parse_from_str(head, "%Y-%m-%dT%H:%M:%S")
        .or_else(|_| chrono::NaiveDateTime::parse_from_str(head, "%Y-%m-%d %H:%M:%S"))
        .ok()?;
    Some(naive.and_utc().timestamp() - 19_800)
}

/// Makes the day's token from the PIN and a TOTP code: the token and when it ends (blocking).
pub fn generate(c: &Credentials, urls: &Urls, now: i64) -> Result<(String, i64), String> {
    if let Some(name) = c.missing() {
        return Err(format!("Set {name} in Settings > Plugins > Dhan"));
    }
    let code = opentrader_plugin_kit::totp::code(&c.totp_secret, now)?;
    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).timeout_global(Some(Duration::from_secs(30))).build().into();
    let url = format!("{}/app/generateAccessToken?dhanClientId={}&pin={}&totp={code}", urls.auth, c.client_id, c.pin);
    let resp = agent.post(&url).header("Accept", "application/json").send_empty().map_err(|e| format!("Dhan token: {e}"))?;
    let status = resp.status().as_u16();
    let text = resp.into_body().read_to_string().map_err(|e| format!("Dhan token: {e}"))?;
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    match v["accessToken"].as_str().filter(|t| !t.is_empty()) {
        Some(token) => {
            let ends = v["expiryTime"].as_str().and_then(expiry_of).unwrap_or(now + 23 * 3600);
            Ok((token.to_string(), ends))
        }
        None => {
            let why = ["message", "errorMessage", "remarks", "error"].iter().find_map(|k| v[*k].as_str()).map(str::to_string).unwrap_or_else(|| format!("HTTP {status}"));
            Err(format!("Dhan token: {why}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_credentials_are_named() {
        let mut c = Credentials { client_id: "1100".into(), ..Default::default() };
        assert_eq!(c.missing(), Some("DHAN_PIN (or DHAN_ACCESS_TOKEN)"));
        c.pin = "123456".into();
        assert_eq!(c.missing(), Some("DHAN_TOTP_SECRET (or DHAN_ACCESS_TOKEN)"));
        c.totp_secret = "GEZDGNBV".into();
        assert_eq!(c.missing(), None);
        let given = Credentials { client_id: "1100".into(), access_token: "eyJ".into(), ..Default::default() };
        assert_eq!(given.missing(), None);
        assert_eq!(generate(&Credentials::default(), &Urls::default(), 0).unwrap_err(), "Set DHAN_CLIENT_ID in Settings > Plugins > Dhan");
    }

    #[test]
    fn expiry_times_are_read() {
        assert_eq!(expiry_of("2025-11-14T09:15:00+05:30"), Some(1_763_091_900));
        assert_eq!(expiry_of("2025-11-14T09:15:00.000"), Some(1_763_091_900));
        assert_eq!(expiry_of("soon"), None);
    }
}
