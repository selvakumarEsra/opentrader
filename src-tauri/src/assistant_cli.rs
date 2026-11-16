/*
 * The AI assistant's SDK models (specs/assistant-cli-models.md): Claude Code (Claude Agent SDK), Codex (Codex SDK) and
 * Gemini (Google Gen AI SDK), each run on this computer with the user's own credentials.
 *
 * The SDKs are Node.js libraries: they run in `assistant-bridge/` (bridge.mjs and its package files, built into the
 * app), which the first question to one of them installs with npm into the app's data folder (and again when this
 * version's package files differ from the installed ones). Each question starts `node bridge.mjs`, gives it the
 * question on stdin and passes its JSON events on to the window; Stop kills it (and what it started).
 *
 * The model reaches the app's tools over the MCP endpoint of Settings > Coding agents (agent.rs), opened for the
 * question only with a token of its own. Tokens typed in Settings > Coding agents are kept here (assistant-models.json,
 * readable by the user only); a token in the environment (the login shell's, as the app may be started without it)
 * is used when none is typed.
 */
use opentrader_core::files;
use opentrader_core::sync::LockExt;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tauri::ipc::Channel;
use tauri::{AppHandle, Manager};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::oneshot;

const FILE: &str = "assistant-models.json";
const BRIDGE_DIR: &str = "assistant-bridge";
const BRIDGE_JS: &str = include_str!("../assistant-bridge/bridge.mjs");
const PACKAGE_JSON: &str = include_str!("../assistant-bridge/package.json");
const PACKAGE_LOCK: &str = include_str!("../assistant-bridge/package-lock.json");
/// Written once npm installed this version's package files.
const STAMP: &str = ".installed";
/// The npm install of the SDKs (they bring the agents' programs): a slow connection takes minutes.
const INSTALL_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// The error output kept to explain a failed run.
const STDERR_LINES: usize = 20;

/// The SDK models: id, name, the token's environment variables (the first is the one the SDK reads).
struct Provider {
    id: &'static str,
    name: &'static str,
    env: &'static [&'static str],
    need: &'static str,
}

const PROVIDERS: [Provider; 3] = [
    Provider {
        id: "claude",
        name: "Claude Code",
        env: &["CLAUDE_CODE_OAUTH_TOKEN", "CLAUDE_CODE_TOKEN", "ANTHROPIC_API_KEY"],
        need: "a Claude Code token (claude setup-token) or a Claude Code login on this computer",
    },
    Provider {
        id: "codex",
        name: "Codex",
        env: &["CODEX_API_KEY", "OPENAI_API_KEY"],
        need: "an OpenAI API key (CODEX_API_KEY) or a Codex login on this computer",
    },
    Provider { id: "gemini", name: "Gemini", env: &["GEMINI_API_KEY", "GOOGLE_API_KEY"], need: "a Gemini API key (GEMINI_API_KEY)" },
];

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct Config {
    /// Per model id: the token typed in Settings.
    tokens: HashMap<String, String>,
    /// Per model id: the model name asked of it (empty: the SDK's default).
    models: HashMap<String, String>,
}

/// One SDK model as Settings and the assistant panel show it.
#[derive(Clone, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct CliModel {
    pub id: String,
    pub name: String,
    /// Has credentials, and Node.js is there.
    pub ready: bool,
    /// What it lacks, when not ready.
    pub need: Option<String>,
    /// Where its credentials come from ("token typed in Settings", "CLAUDE_CODE_OAUTH_TOKEN", "Codex login"…).
    pub source: Option<String>,
    pub token_set: bool,
    /// The model name asked of it (empty: the SDK's default).
    pub model: String,
}

#[derive(Clone, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct CliStatus {
    /// Node.js (its path), which runs the SDKs.
    pub node: Option<String>,
    /// The SDKs are installed for this version.
    pub installed: bool,
    pub models: Vec<CliModel>,
}

fn config_path(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_config_dir().ok().map(|d| d.join(FILE))
}

fn read_config(app: &AppHandle) -> Config {
    config_path(app).map(|p| files::load_or_default(&p)).unwrap_or_default()
}

fn write_config(app: &AppHandle, config: &Config) -> Result<(), String> {
    let path = config_path(app).ok_or("No config folder")?;
    files::save_json(&path, config).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
    }
    Ok(())
}

// ── The login shell's environment ────────────────────────────────────────
/// The variables the SDKs need from the user's environment: an app started from the Dock or a menu has none of what
/// the login shell sets (its PATH, its exported tokens).
fn shell_env() -> &'static HashMap<String, String> {
    static ENV: OnceLock<HashMap<String, String>> = OnceLock::new();
    ENV.get_or_init(|| {
        let mut env: HashMap<String, String> = std::env::vars().collect();
        #[cfg(unix)]
        if let Some(found) = login_shell_env() {
            for (k, v) in found {
                if k == "PATH" || PROVIDERS.iter().any(|p| p.env.contains(&k.as_str())) {
                    env.insert(k, v);
                }
            }
        }
        env
    })
}

const ENV_MARK: &str = "__OPENTRADER_ENV__";

/// `env` as the login shell sets it (5 s at most: a shell asking for input is given up).
#[cfg(unix)]
fn login_shell_env() -> Option<HashMap<String, String>> {
    let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/sh".into());
    let mut child = std::process::Command::new(shell)
        .args(["-ilc", &format!("printf '\\n{ENV_MARK}\\n'; env")])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() < Duration::from_secs(5) => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                return None;
            }
        }
    }
    let mut out = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take()?, &mut out).ok()?;
    Some(parse_env(&out))
}

/// The `KEY=value` lines after the mark (what the shell's start-up files printed before it is left out).
fn parse_env(out: &str) -> HashMap<String, String> {
    let after = out.split_once(ENV_MARK).map_or("", |(_, a)| a);
    after
        .lines()
        .filter_map(|l| l.split_once('='))
        .filter(|(k, _)| !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from)
}

// ── Node.js ──────────────────────────────────────────────────────────────
#[cfg(windows)]
const NODE: &str = "node.exe";
#[cfg(not(windows))]
const NODE: &str = "node";
#[cfg(windows)]
const NPM: &str = "npm.cmd";
#[cfg(not(windows))]
const NPM: &str = "npm";

/// Node.js: on the login shell's PATH, else where installers put it (Homebrew, nvm, Volta).
fn node_path() -> Option<PathBuf> {
    let on_path = shell_env().get("PATH").into_iter().flat_map(|p| std::env::split_paths(p).collect::<Vec<_>>());
    let mut places: Vec<PathBuf> = on_path.collect();
    places.extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"].map(PathBuf::from));
    if let Some(h) = home() {
        places.push(h.join(".volta/bin"));
        // The newest nvm version.
        if let Ok(rd) = std::fs::read_dir(h.join(".nvm/versions/node")) {
            let mut versions: Vec<PathBuf> = rd.flatten().map(|e| e.path().join("bin")).collect();
            versions.sort();
            places.extend(versions.into_iter().rev());
        }
    }
    places.into_iter().map(|d| d.join(NODE)).find(|p| p.is_file())
}

/// The PATH the SDK programs run with: Node's folder first, then the login shell's.
fn run_path(node: &Path) -> String {
    let mut dirs: Vec<PathBuf> = node.parent().map(Path::to_path_buf).into_iter().collect();
    if let Some(p) = shell_env().get("PATH") {
        dirs.extend(std::env::split_paths(p));
    }
    std::env::join_paths(dirs).map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()
}

// ── Credentials ──────────────────────────────────────────────────────────
/// A program on the login shell's PATH.
fn on_path(name: &str) -> bool {
    shell_env().get("PATH").is_some_and(|p| std::env::split_paths(p).any(|d| d.join(name).is_file() || d.join(format!("{name}.exe")).is_file()))
}

/// `GEMINI_API_KEY` of Gemini CLI's `~/.gemini/.env`, where its users keep it.
fn gemini_cli_key() -> Option<String> {
    let text = std::fs::read_to_string(home()?.join(".gemini/.env")).ok()?;
    text.lines().find_map(|l| {
        let l = l.trim().trim_start_matches("export ");
        let v = l.strip_prefix("GEMINI_API_KEY=")?.trim().trim_matches('"').trim_matches('\'');
        (!v.is_empty()).then(|| v.to_string())
    })
}

/// Where a model's credentials come from, if anywhere.
fn credentials_source(p: &Provider, config: &Config) -> Option<String> {
    if config.tokens.get(p.id).is_some_and(|t| !t.is_empty()) {
        return Some("token typed in Settings".into());
    }
    let env = shell_env();
    if let Some(k) = p.env.iter().find(|k| env.get(**k).is_some_and(|v| !v.is_empty())) {
        return Some((*k).to_string());
    }
    let home = home();
    match p.id {
        // Claude Code keeps its login in the system keychain (macOS) or ~/.claude/.credentials.json.
        "claude" if on_path("claude") || home.as_ref().is_some_and(|h| h.join(".claude/.credentials.json").is_file()) => Some("Claude Code login".into()),
        "codex" if home.as_ref().is_some_and(|h| h.join(".codex/auth.json").is_file()) => Some("Codex login".into()),
        "gemini" if gemini_cli_key().is_some() => Some("~/.gemini/.env".into()),
        _ => None,
    }
}

/// The environment variables carrying a model's credentials to its SDK.
fn credentials_env(id: &str, config: &Config) -> Vec<(String, String)> {
    let env = shell_env();
    let typed = config.tokens.get(id).filter(|t| !t.is_empty()).cloned();
    let from_env = |k: &str| env.get(k).filter(|v| !v.is_empty()).cloned();
    let mut out = Vec::new();
    match id {
        "claude" => {
            let token = typed.or_else(|| from_env("CLAUDE_CODE_OAUTH_TOKEN")).or_else(|| from_env("CLAUDE_CODE_TOKEN"));
            match token {
                // An API key typed in the token's place.
                Some(t) if t.starts_with("sk-ant-api") => out.push(("ANTHROPIC_API_KEY".into(), t)),
                Some(t) => out.push(("CLAUDE_CODE_OAUTH_TOKEN".into(), t)),
                None => {
                    if let Some(k) = from_env("ANTHROPIC_API_KEY") {
                        out.push(("ANTHROPIC_API_KEY".into(), k));
                    }
                }
            }
        }
        "codex" => {
            if let Some(k) = typed.or_else(|| from_env("CODEX_API_KEY")).or_else(|| from_env("OPENAI_API_KEY")) {
                out.push(("CODEX_API_KEY".into(), k));
            }
        }
        "gemini" => {
            if let Some(k) = typed.or_else(|| from_env("GEMINI_API_KEY")).or_else(|| from_env("GOOGLE_API_KEY")).or_else(gemini_cli_key) {
                out.push(("GEMINI_API_KEY".into(), k));
            }
        }
        _ => {}
    }
    out
}

fn bridge_dir(app: &AppHandle) -> Result<PathBuf, String> {
    app.path().app_data_dir().map(|d| d.join(BRIDGE_DIR)).map_err(|e| e.to_string())
}

/// This version's package files, as the stamp of an install made from them.
fn package_stamp() -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(PACKAGE_JSON.as_bytes());
    h.update(PACKAGE_LOCK.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn installed(dir: &Path) -> bool {
    std::fs::read_to_string(dir.join(STAMP)).is_ok_and(|s| s.trim() == package_stamp()) && dir.join("node_modules").is_dir()
}

fn status(app: &AppHandle) -> CliStatus {
    let config = read_config(app);
    let node = node_path();
    let models = PROVIDERS
        .iter()
        .map(|p| {
            let source = credentials_source(p, &config);
            let need = if node.is_none() {
                Some("Node.js 18 or newer (nodejs.org)".to_string())
            } else if source.is_none() {
                Some(p.need.to_string())
            } else {
                None
            };
            CliModel {
                id: p.id.into(),
                name: p.name.into(),
                ready: need.is_none(),
                need,
                source,
                token_set: config.tokens.get(p.id).is_some_and(|t| !t.is_empty()),
                model: config.models.get(p.id).cloned().unwrap_or_default(),
            }
        })
        .collect();
    CliStatus { node: node.map(|p| p.to_string_lossy().into_owned()), installed: bridge_dir(app).is_ok_and(|d| installed(&d)), models }
}

/// The SDK models: which are ready, what the others need.
#[tauri::command]
#[specta::specta]
pub async fn assistant_cli_status(app: AppHandle) -> CliStatus {
    // The login shell is asked once, off the main thread.
    tauri::async_runtime::spawn_blocking(move || status(&app)).await.unwrap_or_else(|_| CliStatus { node: None, installed: false, models: Vec::new() })
}

/// Sets (or, with an empty text, clears) a model's token and the model name asked of it; None leaves one as it is.
#[tauri::command]
#[specta::specta]
pub async fn assistant_cli_configure(app: AppHandle, id: String, token: Option<String>, model: Option<String>) -> Result<CliStatus, String> {
    if !PROVIDERS.iter().any(|p| p.id == id) {
        return Err(format!("Unknown model {id}"));
    }
    let mut config = read_config(&app);
    if let Some(t) = token {
        let t = t.trim().to_string();
        if t.is_empty() {
            config.tokens.remove(&id);
        } else {
            config.tokens.insert(id.clone(), t);
        }
    }
    if let Some(m) = model {
        let m = m.trim().to_string();
        if m.is_empty() {
            config.models.remove(&id);
        } else {
            config.models.insert(id.clone(), m);
        }
    }
    write_config(&app, &config)?;
    Ok(assistant_cli_status(app).await)
}

// ── Install ──────────────────────────────────────────────────────────────
/// One install at a time (two first questions at once would run npm twice in the same folder).
static INSTALLING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The bridge's files in the data folder, and the SDKs installed for them (npm, the first time and after an update
/// that changed them). `say` tells the window what is going on.
async fn ensure_installed(app: &AppHandle, node: &Path, say: &impl Fn(&str)) -> Result<PathBuf, String> {
    let _one = INSTALLING.lock().await;
    let dir = bridge_dir(app)?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    files::write_atomic(&dir.join("bridge.mjs"), BRIDGE_JS.as_bytes()).map_err(|e| e.to_string())?;
    if installed(&dir) {
        return Ok(dir);
    }
    say("Setting up the AI models' SDKs on this computer (first use, a few minutes)…");
    files::write_atomic(&dir.join("package.json"), PACKAGE_JSON.as_bytes()).map_err(|e| e.to_string())?;
    files::write_atomic(&dir.join("package-lock.json"), PACKAGE_LOCK.as_bytes()).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(dir.join(STAMP));
    let npm = node.parent().map(|d| d.join(NPM)).filter(|p| p.is_file()).ok_or("npm was not found next to Node.js")?;
    let mut cmd = tokio::process::Command::new(npm);
    cmd.args(["ci", "--omit=dev", "--no-audit", "--no-fund", "--loglevel=error"])
        .current_dir(&dir)
        .env("PATH", run_path(node))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let out = tokio::time::timeout(INSTALL_TIMEOUT, cmd.output()).await.map_err(|_| "Setting up the SDKs took too long".to_string())?;
    let out = out.map_err(|e| format!("npm could not start: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let tail: Vec<&str> = err.lines().rev().take(6).collect::<Vec<_>>().into_iter().rev().collect();
        return Err(format!("Setting up the SDKs failed: {}", tail.join(" ")));
    }
    files::write_atomic(&dir.join(STAMP), package_stamp().as_bytes()).map_err(|e| e.to_string())?;
    Ok(dir)
}

// ── Runs ─────────────────────────────────────────────────────────────────
/// The questions under way: their stop.
static RUNNING: Mutex<Option<HashMap<u32, oneshot::Sender<()>>>> = Mutex::new(None);
/// The bridge programs running (their process ids), killed when the app quits.
static CHILDREN: Mutex<Option<HashMap<u32, u32>>> = Mutex::new(None);

/// A question's entries, removed however it ends.
struct Running(u32);

impl Running {
    fn new(request: u32) -> (Self, oneshot::Receiver<()>) {
        let (tx, rx) = oneshot::channel();
        RUNNING.locked().get_or_insert_with(HashMap::new).insert(request, tx);
        (Running(request), rx)
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(r) = RUNNING.locked().as_mut() {
            r.remove(&self.0);
        }
        if let Some(c) = CHILDREN.locked().as_mut() {
            c.remove(&self.0);
        }
    }
}

/// The app's MCP endpoint opened for one question, closed however it ends.
struct McpRun(AppHandle, String);

impl Drop for McpRun {
    fn drop(&mut self) {
        crate::agent::end_run(&self.0, &self.1);
    }
}

/// Ends a bridge program and every process it started (its process group).
fn kill_tree(pid: u32) {
    #[cfg(unix)]
    {
        let _ = std::process::Command::new("kill").args(["-TERM", &format!("-{pid}")]).status();
    }
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill").args(["/PID", &pid.to_string(), "/T", "/F"]).status();
    }
}

/// The bridge's events of one question, as `on_event` gets them.
fn event(t: &str, key: &str, text: &str) -> String {
    serde_json::json!({ "t": t, key: text }).to_string()
}

/// What the window sends for a question.
#[derive(Deserialize)]
struct Question {
    system: String,
    prompt: String,
    #[serde(default = "yes")]
    web: bool,
}

fn yes() -> bool {
    true
}

/// One question to SDK model `id` (`body`: the system text, the prompt with the chat so far, web on or off). Each
/// event of the bridge is sent on `on_event` (its JSON text). Returns when the answer is complete, stopped
/// (assistant_cli_cancel) or failed.
#[tauri::command]
#[specta::specta]
pub async fn assistant_cli_chat(app: AppHandle, window: tauri::Window, request: u32, id: String, body: String, on_event: Channel<String>) -> Result<(), String> {
    let q: Question = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    let provider = PROVIDERS.iter().find(|p| p.id == id).ok_or_else(|| format!("Unknown model {id}"))?;
    let (_running, mut stop_rx) = Running::new(request);
    let send = |s: String| {
        let _ = on_event.send(s);
    };
    let say = |text: &str| send(event("status", "text", text));
    let node = node_path().ok_or("Node.js 18 or newer is needed to run this model (nodejs.org)")?;
    let config = read_config(&app);
    if credentials_source(provider, &config).is_none() {
        return Err(format!("{} needs {}: set it in Settings > Coding agents", provider.name, provider.need));
    }
    let dir = tokio::select! {
        r = ensure_installed(&app, &node, &say) => r?,
        _ = &mut stop_rx => return Ok(()),
    };
    let (url, token) = crate::agent::begin_run(&app, window.label())?;
    let _mcp = McpRun(app.clone(), token.clone());

    let input = serde_json::json!({
        "provider": provider.id,
        "model": config.models.get(provider.id).cloned().unwrap_or_default(),
        "system": q.system,
        "prompt": q.prompt,
        "web": q.web,
        "mcp": { "url": url },
    });
    let mut cmd = tokio::process::Command::new(&node);
    cmd.arg(dir.join("bridge.mjs"))
        .current_dir(&dir)
        .env("PATH", run_path(&node))
        .env("OPENTRADER_MCP_TOKEN", token)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // The tokens of the other models stay out of this one's environment.
    for p in &PROVIDERS {
        for k in p.env {
            cmd.env_remove(k);
        }
    }
    for (k, v) in credentials_env(provider.id, &config) {
        cmd.env(k, v);
    }
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = cmd.spawn().map_err(|e| format!("Node.js could not start: {e}"))?;
    let pid = child.id().unwrap_or(0);
    CHILDREN.locked().get_or_insert_with(HashMap::new).insert(request, pid);
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(input.to_string().as_bytes()).await.map_err(|e| e.to_string())?;
        drop(stdin);
    }
    let mut out = BufReader::new(child.stdout.take().ok_or("no output")?).lines();
    let err = child.stderr.take().ok_or("no error output")?;
    let errors = tokio::spawn(async move {
        let mut lines = BufReader::new(err).lines();
        let mut tail: Vec<String> = Vec::new();
        while let Ok(Some(l)) = lines.next_line().await {
            // Node's warnings are not the error.
            if l.contains("Warning:") || l.contains("--trace-warnings") {
                continue;
            }
            tail.push(l);
            if tail.len() > STDERR_LINES {
                tail.remove(0);
            }
        }
        tail
    });
    let mut ended = false;
    loop {
        tokio::select! {
            line = out.next_line() => match line {
                Ok(Some(l)) if l.trim().is_empty() => {}
                Ok(Some(l)) => {
                    let t = serde_json::from_str::<serde_json::Value>(&l).ok().and_then(|v| v["t"].as_str().map(String::from));
                    match t.as_deref() {
                        Some("error") | Some("done") => ended = true,
                        Some(_) => {}
                        // A line that is not an event (a library writing to stdout) is not passed on.
                        None => continue,
                    }
                    send(l);
                }
                _ => break,
            },
            _ = &mut stop_rx => {
                kill_tree(pid);
                // What does not end in 1.5 s is killed.
                if tokio::time::timeout(Duration::from_millis(1500), child.wait()).await.is_err() {
                    let _ = child.kill().await;
                }
                return Ok(());
            }
        }
    }
    let status = child.wait().await.map_err(|e| e.to_string())?;
    let tail = errors.await.unwrap_or_default();
    if !ended {
        let why = if tail.is_empty() { format!("The model's program stopped ({status})") } else { tail.join(" ") };
        send(event("error", "message", &why));
    }
    Ok(())
}

/// Stops a question under way (its answer so far stays).
#[tauri::command]
#[specta::specta]
pub fn assistant_cli_cancel(request: u32) {
    if let Some(tx) = RUNNING.locked().as_mut().and_then(|r| r.remove(&request)) {
        let _ = tx.send(());
    }
}

/// The app quits: no bridge program outlives it.
pub fn shutdown() {
    let pids: Vec<u32> = CHILDREN.locked().as_ref().map(|c| c.values().copied().collect()).unwrap_or_default();
    for pid in pids {
        kill_tree(pid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_login_shell_env_is_read_after_the_mark() {
        let out = format!("Welcome!\nPATH=/wrong\n{ENV_MARK}\nPATH=/opt/homebrew/bin:/usr/bin\nCLAUDE_CODE_OAUTH_TOKEN=abc=def\n bad line\n");
        let env = parse_env(&out);
        assert_eq!(env.get("PATH").map(String::as_str), Some("/opt/homebrew/bin:/usr/bin"));
        assert_eq!(env.get("CLAUDE_CODE_OAUTH_TOKEN").map(String::as_str), Some("abc=def"), "only the first = splits");
        assert_eq!(env.len(), 2);
        assert!(parse_env("no mark").is_empty());
    }

    #[test]
    fn a_typed_claude_token_goes_where_it_belongs() {
        let mut c = Config::default();
        c.tokens.insert("claude".into(), "sk-ant-oat01-xyz".into());
        assert_eq!(credentials_env("claude", &c), vec![("CLAUDE_CODE_OAUTH_TOKEN".to_string(), "sk-ant-oat01-xyz".to_string())]);
        c.tokens.insert("claude".into(), "sk-ant-api03-xyz".into());
        assert_eq!(credentials_env("claude", &c), vec![("ANTHROPIC_API_KEY".to_string(), "sk-ant-api03-xyz".to_string())]);
        c.tokens.insert("codex".into(), "sk-proj".into());
        assert_eq!(credentials_env("codex", &c), vec![("CODEX_API_KEY".to_string(), "sk-proj".to_string())]);
    }

    #[test]
    fn a_typed_token_is_the_first_source() {
        let mut c = Config::default();
        c.tokens.insert("gemini".into(), "AIza".into());
        assert_eq!(credentials_source(&PROVIDERS[2], &c).as_deref(), Some("token typed in Settings"));
    }

    #[test]
    fn the_stamp_follows_the_package_files() {
        assert_eq!(package_stamp().len(), 64);
        assert_eq!(package_stamp(), package_stamp());
    }
}
