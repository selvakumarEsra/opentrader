/*
 * A broker's access token, made from the user's credentials: kept in memory and in the plugin's folder (readable by
 * the user only), so a restart or the backfill command reuses the day's token instead of logging in again. A refused
 * login is not tried again for a minute, and new credentials forget both the token and the refusal.
 */
use crate::now;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};
use tokio::sync::Notify;

/// A refused login is not tried again sooner.
pub const LOGIN_RETRY_SECS: i64 = 60;

/// What a broker's token keeps.
pub trait Token: Clone + Serialize + DeserializeOwned + Send + Sync + 'static {
    /// The token sent to the broker.
    fn access(&self) -> &str;
}

pub struct Tokens<C, T> {
    /// The broker's name in messages: `Kite`, `Dhan`.
    broker: &'static str,
    creds: RwLock<C>,
    held: tokio::sync::Mutex<Option<T>>,
    /// The last refused login: when, and why.
    failed: Mutex<Option<(i64, String)>>,
    /// Told when the credentials change (a stream waiting for a token tries again at once).
    pub configured: Notify,
}

fn locked<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The token's file in the plugin's folder.
pub fn token_file() -> Option<PathBuf> {
    crate::store::cache_dir().map(|d| d.join("token.json"))
}

/// Writes `body` to `path` readable by the user only: the file is private before it holds anything.
pub fn write_private(path: &Path, body: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp)?;
    file.write_all(body)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, path)
}

impl<C: Clone + PartialEq + Default, T: Token> Tokens<C, T> {
    pub fn new(broker: &'static str) -> Self {
        Tokens { broker, creds: RwLock::new(C::default()), held: tokio::sync::Mutex::new(None), failed: Mutex::new(None), configured: Notify::new() }
    }

    pub fn creds(&self) -> C {
        self.creds.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Takes new credentials; changed ones forget the token and the last refusal.
    pub async fn set(&self, c: C) {
        let changed = *self.creds.read().unwrap_or_else(|e| e.into_inner()) != c;
        if changed {
            *self.creds.write().unwrap_or_else(|e| e.into_inner()) = c;
            *self.held.lock().await = None;
            *locked(&self.failed) = None;
            self.configured.notify_waiters();
        }
    }

    /// The token: the one in memory or on disk when `usable`, else a new one from `login` (run on a blocking
    /// thread), kept for next time. A login refused less than a minute ago answers its refusal.
    pub async fn get(&self, usable: impl Fn(&T) -> bool, login: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<String, String> {
        let mut held = self.held.lock().await;
        if let Some(t) = held.as_ref().filter(|t| usable(t)) {
            return Ok(t.access().to_string());
        }
        if let Some(t) = token_file().and_then(|f| std::fs::read(f).ok()).and_then(|b| serde_json::from_slice::<T>(&b).ok()).filter(|t| usable(t)) {
            let access = t.access().to_string();
            *held = Some(t);
            return Ok(access);
        }
        if let Some((at, why)) = locked(&self.failed).clone() {
            if now() - at < LOGIN_RETRY_SECS {
                return Err(why);
            }
        }
        match tokio::task::spawn_blocking(login).await.map_err(|e| e.to_string())? {
            Ok(t) => {
                if let Some(f) = token_file() {
                    if let Err(e) = write_private(&f, &serde_json::to_vec(&t).unwrap_or_default()) {
                        println!("[warn] the {} token could not be kept on disk: {e}", self.broker);
                    }
                }
                let access = t.access().to_string();
                *held = Some(t);
                Ok(access)
            }
            Err(e) => {
                *locked(&self.failed) = Some((now(), e.clone()));
                Err(e)
            }
        }
    }

    /// The broker refused `access`: the next `get` logs in again.
    pub async fn invalidate(&self, access: &str) {
        let mut held = self.held.lock().await;
        if held.as_ref().is_some_and(|t| t.access() == access) {
            *held = None;
        }
        if let Some(f) = token_file() {
            let _ = std::fs::remove_file(f);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, serde::Serialize, serde::Deserialize)]
    struct T(String);
    impl Token for T {
        fn access(&self) -> &str {
            &self.0
        }
    }

    #[test]
    fn a_private_file_is_the_users_only() {
        let dir = std::env::temp_dir().join(format!("kit-token-{}", std::process::id()));
        let f = dir.join("token.json");
        write_private(&f, b"one").unwrap();
        write_private(&f, b"two").unwrap();
        assert_eq!(std::fs::read(&f).unwrap(), b"two");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&f).unwrap().permissions().mode() & 0o777, 0o600);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_refused_login_answers_its_refusal_until_the_credentials_change() {
        // The plugin folder is a temporary one.
        let dir = std::env::temp_dir().join(format!("kit-tokens-{}", std::process::id()));
        std::env::set_var("OPENTRADER_KITTEST_CACHE", &dir);
        crate::set_plugin("kittest");
        let t: Tokens<String, T> = Tokens::new("Test");
        t.set("a".into()).await;
        assert_eq!(t.get(|_| true, || Err("refused".into())).await, Err("refused".into()));
        assert_eq!(t.get(|_| true, || Ok(T("x".into()))).await, Err("refused".into()), "not tried again so soon");
        t.set("b".into()).await;
        assert_eq!(t.get(|_| true, || Ok(T("x".into()))).await, Ok("x".into()));
        assert_eq!(t.get(|_| true, || Err("not asked".into())).await, Ok("x".into()));
        t.invalidate("x").await;
        assert_eq!(t.get(|_| true, || Ok(T("y".into()))).await, Ok("y".into()));
        let _ = std::fs::remove_dir_all(dir);
    }
}
