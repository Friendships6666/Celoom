//! Closed CDP browser actor. Only a fresh, private AI profile is ever connected.
//! No browser discovery, arbitrary JavaScript tool, or host input fallback.
use crate::{
    computer::{ComputerAction, ComputerImage, ComputerOutput},
    models::NetworkConfig,
};
use anyhow::{Context, Result, anyhow, bail, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};
use tokio::{
    net::TcpStream,
    process::{Child, Command},
    sync::Mutex,
    time::{Instant, timeout},
};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, tungstenite::Message};
use tokio_util::sync::CancellationToken;

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 800;
const MAX_REFS: usize = 120;
const RPC_TIMEOUT: Duration = Duration::from_secs(8);
const ACTION_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_CDP_MESSAGE: usize = 16 * 1024 * 1024;
type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Clone)]
pub struct BrowserController {
    data_dir: PathBuf,
    slots: Arc<Mutex<HashMap<String, Arc<BrowserSlot>>>>,
    network: Arc<Mutex<NetworkConfig>>,
}
struct BrowserSlot {
    state: Mutex<Option<BrowserSession>>,
    handoff: Mutex<Option<HandoffWindow>>,
}
impl BrowserSlot {
    fn new() -> Self {
        Self {
            state: Mutex::new(None),
            handoff: Mutex::new(None),
        }
    }
}
struct HandoffWindow {
    owner: String,
    child: Child,
    url: String,
}
struct BrowserSession {
    owner: String,
    child: Child,
    profile: PathBuf,
    socket: Socket,
    target: String,
    session: String,
    next_id: u64,
    epoch: u64,
    snapshot: Option<String>,
    refs: HashMap<String, ElementRef>,
    screenshot_epoch: Option<u64>,
    screenshot_stamp: Option<Value>,
    screenshot_at: Option<Instant>,
    healthy: bool,
    sandbox: bool,
    network_route: BrowserRoute,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct BrowserRoute {
    mode: String,
    http: Option<String>,
    https: Option<String>,
    other: Option<String>,
    proxy_server: Option<String>,
    bypass: String,
}
impl BrowserRoute {
    fn flags(&self) -> Vec<String> {
        let mut flags = vec![
            "--disable-quic".into(),
            "--force-webrtc-ip-handling-policy=disable_non_proxied_udp".into(),
        ];
        if let Some(server) = &self.proxy_server {
            flags.push(format!("--proxy-server={server}"));
            // Chrome otherwise silently bypasses local/link-local origins even if the
            // application's explicit proxy configuration has no such exclusion.
            flags.push(format!("--proxy-bypass-list={}", self.bypass));
        } else {
            flags.push("--no-proxy-server".into());
        }
        flags
    }
    fn summary(&self) -> Value {
        json!({"mode":self.mode,"http":self.http.as_deref().unwrap_or("direct"),"https":self.https.as_deref().unwrap_or("direct"),"other":self.other.as_deref().unwrap_or("direct"),"proxy_enabled":self.proxy_server.is_some(),"no_proxy_exclusions":self.bypass.split(';').filter(|s|!s.is_empty()&&*s!="<-loopback>").count(),"implicit_loopback_bypass":false,"direct_fallback_on_proxy_failure":false,"cdp":"loopback-direct","scope":"Chrome HTTP(S)/WebSocket URL requests, not an OS VPN","quic_disabled":true,"webrtc_policy":"disable_non_proxied_udp"})
    }
}
#[derive(Clone)]
struct ElementRef {
    backend: i64,
    epoch: u64,
    fingerprint: Value,
    ax_identity: Option<(String, String)>,
}
impl Drop for BrowserSession {
    fn drop(&mut self) {
        // The child owns a dedicated process group; never signal an attached browser.
        if let Some(pid) = self.child.id() {
            let _ = nix::sys::signal::killpg(
                nix::unistd::Pid::from_raw(pid as i32),
                nix::sys::signal::Signal::SIGKILL,
            );
            let _ = self.child.start_kill();
        }
    }
}
impl BrowserController {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            slots: Arc::new(Mutex::new(HashMap::new())),
            network: Arc::new(Mutex::new(NetworkConfig::default())),
        }
    }
    async fn slot_for(&self, owner: &str) -> Arc<BrowserSlot> {
        let mut slots = self.slots.lock().await;
        slots
            .entry(owner.to_owned())
            .or_insert_with(|| Arc::new(BrowserSlot::new()))
            .clone()
    }
    pub async fn configure_network(&self, network: NetworkConfig) -> Result<()> {
        let _ = resolve_browser_route(&network, process_env)?;
        *self.network.lock().await = network;
        Ok(())
    }
    pub async fn status(&self) -> Value {
        let binary = browser_binary();
        let slots: Vec<(String, Arc<BrowserSlot>)> = self
            .slots
            .lock()
            .await
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let mut running = false;
        let mut owners = Vec::new();
        let mut sandbox = None;
        for (id, slot) in &slots {
            if let Ok(st) = slot.state.try_lock()
                && let Some(s) = st.as_ref()
                && s.healthy
            {
                running = true;
                owners.push(id.clone());
                sandbox = Some(s.sandbox);
            }
        }
        let network = match self.network.try_lock() {
            Ok(config) => match resolve_browser_route(&config, process_env) {
                Ok(route) => route.summary(),
                Err(error) => json!({"valid":false,"error":error.to_string()}),
            },
            Err(_) => json!({"busy":true}),
        };
        json!({"available":binary.is_some(),"binary":binary,"running":running,"busy":false,"network":network,
            "owner":owners.first().cloned(),"owners":owners,"active_count":owners.len(),"isolation":"exclusive_per_session_profile",
            "host_input":false,"attached":false,"viewport":{"width":WIDTH,"height":HEIGHT},
            "sandbox":sandbox,
            "capabilities":["open","observe","click","fill","type","key","scroll","hover","back","wait","eval","cookies","handoff","resume_headless","screenshot","stop"],
            "limitations":["Each session owns an exclusive isolated browser instance; unlimited concurrent sessions supported.","Persistent cookies imported via settings are shared into each new browser instance."]})
    }
    pub async fn stop(&self, owner: Option<&str>) -> Result<()> {
        let targets: Vec<Arc<BrowserSlot>> = match owner {
            Some(id) => {
                let slots = self.slots.lock().await;
                slots.get(id).cloned().into_iter().collect()
            }
            None => self.slots.lock().await.values().cloned().collect(),
        };
        for slot in targets {
            let mut guard = slot.state.lock().await;
            if let Some(mut session) = guard.take() {
                terminate(&mut session).await;
            }
            drop(guard);
            close_slot_handoff(&slot).await;
        }
        Ok(())
    }
    async fn close_handoff(slot: &BrowserSlot) -> Option<String> {
        close_slot_handoff(slot).await
    }
    pub async fn execute(
        &self,
        owner: &str,
        action: &ComputerAction,
        cancel: CancellationToken,
    ) -> Result<ComputerOutput> {
        if action.action == "status" {
            return output(self.status().await);
        }
        if action.action == "stop" {
            self.stop(Some(owner)).await?;
            return output(json!({"ok":true,"running":false,"outcome":"applied"}));
        }
        ensure!(!cancel.is_cancelled(), "CANCELLED: action not started");
        ensure!(
            matches!(
                action.action.as_str(),
                "open" | "observe" | "click" | "fill" | "type" | "key" | "scroll" | "hover" | "back" | "wait" | "eval" | "cookies" | "handoff" | "resume_headless" | "screenshot"
            ),
            "Unsupported browser action"
        );
        let slot = self.slot_for(owner).await;
        let mut guard = slot.state.lock().await;
        if action.action == "handoff" {
            let mut target_url = action.url.clone();
            if target_url.is_none()
                && let Some(session) = guard.as_mut()
                && let Ok(loc) = session.location(&cancel).await
            {
                target_url = loc["url"].as_str().map(str::to_string);
            }
            let target_url = target_url
                .filter(|u| !u.trim().is_empty() && u != "about:blank")
                .unwrap_or_else(|| "https://www.jd.com".to_string());
            validate_url(&target_url)?;
            if let Some(mut session) = guard.take() {
                terminate(&mut session).await;
            }
            Self::close_handoff(&slot).await;
            let binary = browser_binary().context(
                "BROWSER_UNAVAILABLE: install Chromium/Chrome or set absolute CELOOM_BROWSER_PATH",
            )?;
            let profile = profile_for(&self.data_dir, owner).await?;
            for lock_file in [
                "DevToolsActivePort",
                "SingletonLock",
                "SingletonSocket",
                "SingletonCookie",
            ] {
                let _ = tokio::fs::remove_file(profile.join(lock_file)).await;
            }
            let route = resolve_browser_route(&*self.network.lock().await, process_env)?;
            let mut cmd = Command::new(binary);
            cmd.env_clear();
            for name in [
                "HOME",
                "PATH",
                "LANG",
                "LC_ALL",
                "TZ",
                "DISPLAY",
                "WAYLAND_DISPLAY",
                "XAUTHORITY",
                "DBUS_SESSION_BUS_ADDRESS",
                "XDG_RUNTIME_DIR",
                "TMPDIR",
            ] {
                if let Some(value) = std::env::var_os(name) {
                    cmd.env(name, value);
                }
            }
            cmd.args([
                "--no-first-run",
                "--no-default-browser-check",
                "--disable-sync",
                "--disable-blink-features=AutomationControlled",
                "--lang=zh-CN",
                "--password-store=basic",
                "--window-size=1280,860",
                "--new-window",
            ])
            .args(route.flags())
            .arg(format!("--user-data-dir={}", profile.display()))
            .arg(&target_url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .process_group(0);
            if crate::privilege::uid() == 0
                && std::env::var("CELOOM_BROWSER_ALLOW_NO_SANDBOX").as_deref() == Ok("1")
            {
                cmd.arg("--no-sandbox");
            }
            let child = cmd.spawn().context("Cannot launch visible browser window for handoff")?;
            *slot.handoff.lock().await = Some(HandoffWindow {
                owner: owner.to_owned(),
                child,
                url: target_url.clone(),
            });
            return output(json!({
                "ok": true,
                "action": "handoff",
                "url": target_url,
                "message": "Visible browser window opened on your desktop. Call ask_user(blocking=true) now to wait for the user to complete CAPTCHA/login and click Continue; then call computer(target='browser', action='resume_headless') to close the popup and resume headless."
            }));
        }
        let resumed_url = Self::close_handoff(&slot).await;
        if guard.is_none() {
            if action.action == "cookies" {
                return output(self.cookie_summary().await);
            }
            if action.action == "resume_headless" {
                let target_url = action
                    .url
                    .clone()
                    .or(resumed_url)
                    .unwrap_or_else(|| "about:blank".to_string());
                let route = resolve_browser_route(&*self.network.lock().await, process_env)?;
                *guard = Some(launch(&self.data_dir, owner, &route, &cancel).await?);
                let session = guard.as_mut().unwrap();
                let open_act = ComputerAction {
                    action: "open".into(),
                    target: Some("browser".into()),
                    ref_id: None,
                    url: Some(target_url),
                    text: None,
                    x: None,
                    y: None,
                    key: None,
                    button: None,
                    delta: None,
                    argv: None,
                    max_nodes: None,
                    max_depth: None,
                    color: None,
                };
                return session.perform(&open_act, &cancel).await;
            }
            ensure!(
                action.action == "open",
                "BROWSER_NOT_RUNNING: use open first"
            );
            // Validate before creating a process/profile.
            validate_url(action.url.as_deref().context("open requires url")?)?;
            let route = resolve_browser_route(&*self.network.lock().await, process_env)?;
            *guard = Some(launch(&self.data_dir, owner, &route, &cancel).await?);
        }
        let session = guard.as_mut().unwrap();
        let result = tokio::select! {
            _ = cancel.cancelled() => { session.healthy = false; Err(anyhow!("CANCELLED: browser action outcome may be unknown; not replayed")) },
            r = timeout(ACTION_TIMEOUT, session.perform(action, &cancel)) => match r {
                Ok(r) => r,
                Err(_) => { session.healthy = false; Err(anyhow!("BROWSER_TIMEOUT: outcome unknown; not replayed")) }
            }
        };
        if !guard.as_ref().unwrap().healthy
            && let Some(mut old) = guard.take()
        {
            terminate(&mut old).await;
        }
        result
    }
    fn cookies_path(&self) -> PathBuf {
        self.data_dir.join("computer").join("imported-cookies.json")
    }
    pub async fn cookie_summary(&self) -> Value {
        let path = self.cookies_path();
        let Ok(raw) = tokio::fs::read_to_string(&path).await else {
            return json!({"count":0,"domains":[]});
        };
        let Ok(list) = serde_json::from_str::<Vec<Value>>(&raw) else {
            return json!({"count":0,"domains":[]});
        };
        let mut domains: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for c in &list {
            if let Some(d) = c["domain"].as_str().or_else(|| c["url"].as_str()) {
                domains.insert(d.to_string());
            }
        }
        json!({"count": list.len(), "domains": domains.into_iter().collect::<Vec<_>>()})
    }
    pub async fn clear_cookies(&self) -> Result<()> {
        let _ = tokio::fs::remove_file(self.cookies_path()).await;
        let slots: Vec<Arc<BrowserSlot>> = self.slots.lock().await.values().cloned().collect();
        let cancel = CancellationToken::new();
        for slot in slots {
            let mut guard = slot.state.lock().await;
            if let Some(session) = guard.as_mut() {
                let _ = session
                    .rpc("Network.clearBrowserCookies", json!({}), &cancel)
                    .await;
            }
        }
        Ok(())
    }
    pub async fn import_cookies(
        &self,
        cookies_json: Option<&str>,
        cookie_header: Option<&str>,
        url: Option<&str>,
    ) -> Result<Value> {
        let mut parsed_cookies: Vec<Value> = Vec::new();
        if let Some(raw_json) = cookies_json.map(str::trim).filter(|s| !s.is_empty()) {
            let arr: Vec<Value> =
                serde_json::from_str(raw_json).context("Cookies JSON must be a JSON array")?;
            for item in arr {
                let Some(name) = item["name"].as_str() else {
                    continue;
                };
                let Some(value) = item["value"].as_str() else {
                    continue;
                };
                let mut c = json!({ "name": name, "value": value });
                if let Some(d) = item["domain"].as_str() {
                    c["domain"] = json!(d);
                } else if let Some(u) = url {
                    c["url"] = json!(u);
                }
                if let Some(p) = item["path"].as_str() {
                    c["path"] = json!(p);
                } else {
                    c["path"] = json!("/");
                }
                if let Some(s) = item["secure"].as_bool() {
                    c["secure"] = json!(s);
                }
                if let Some(h) = item["httpOnly"].as_bool() {
                    c["httpOnly"] = json!(h);
                }
                if let Some(exp) = item["expirationDate"].as_f64() {
                    c["expires"] = json!(exp);
                }
                parsed_cookies.push(c);
            }
        } else if let Some(header) = cookie_header.map(str::trim).filter(|s| !s.is_empty()) {
            let target_url = url
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .context("Cookie header import requires target url (e.g. https://www.jd.com)")?;
            validate_url(target_url)?;
            let clean = header
                .strip_prefix("Cookie:")
                .or_else(|| header.strip_prefix("cookie:"))
                .unwrap_or(header);
            for pair in clean.split(';') {
                let Some((k, v)) = pair.trim().split_once('=') else {
                    continue;
                };
                let name = k.trim();
                if name.is_empty() {
                    continue;
                }
                parsed_cookies.push(json!({
                    "name": name,
                    "value": v.trim(),
                    "url": target_url,
                }));
            }
        } else {
            bail!("Provide cookies_json or cookie_header");
        }
        ensure!(!parsed_cookies.is_empty(), "No valid cookies found");
        ensure!(parsed_cookies.len() <= 500, "Too many cookies (max 500)");

        let dir = self.data_dir.join("computer");
        tokio::fs::create_dir_all(&dir).await?;
        let path = self.cookies_path();
        let bytes = serde_json::to_vec(&parsed_cookies)?;
        tokio::fs::write(&path, &bytes).await?;
        use std::os::unix::fs::PermissionsExt;
        let _ = tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).await;

        let slots: Vec<Arc<BrowserSlot>> = self.slots.lock().await.values().cloned().collect();
        let cancel = CancellationToken::new();
        for slot in slots {
            let mut guard = slot.state.lock().await;
            if let Some(session) = guard.as_mut() {
                let _ = session
                    .rpc(
                        "Network.setCookies",
                        json!({ "cookies": parsed_cookies }),
                        &cancel,
                    )
                    .await;
            }
        }
        let mut summary = self.cookie_summary().await;
        summary["ok"] = json!(true);
        Ok(summary)
    }
}
fn process_env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}
fn proxy_env(env: &impl Fn(&str) -> Option<String>, names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| {
        env(name)
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.trim().to_owned())
    })
}
fn normalize_browser_proxy(value: &str) -> Result<String> {
    let value = value.trim();
    ensure!(
        !value.is_empty()
            && value.len() <= 8192
            && !value
                .chars()
                .any(|c| c.is_control() || c.is_whitespace() || ",;=\\".contains(c)),
        "BROWSER_PROXY_INVALID: expected one HTTP(S) or SOCKS5 proxy URL"
    );
    let url = url::Url::parse(value)
        .map_err(|_| anyhow!("BROWSER_PROXY_INVALID: expected an absolute proxy URL"))?;
    ensure!(
        url.username().is_empty() && url.password().is_none() && !value.contains('@'),
        "BROWSER_PROXY_AUTH_UNSUPPORTED: Chromium flags do not support proxy URL credentials; use an explicitly configured unauthenticated relay"
    );
    let scheme = match url.scheme() {
        "http" => "http",
        "https" => "https",
        "socks5" | "socks5h" => "socks5",
        _ => bail!("BROWSER_PROXY_INVALID: supported proxy schemes are HTTP(S) and SOCKS5"),
    };
    ensure!(
        url.query().is_none() && url.fragment().is_none() && matches!(url.path(), "" | "/"),
        "BROWSER_PROXY_INVALID: proxy URL must not contain path, query or fragment"
    );
    let host = url
        .host_str()
        .context("BROWSER_PROXY_INVALID: proxy host is missing")?;
    let host = if host.contains(':') {
        format!("[{}]", host.trim_start_matches('[').trim_end_matches(']'))
    } else {
        host.to_owned()
    };
    let port = url.port().unwrap_or(match scheme {
        "http" => 80,
        "https" => 443,
        _ => 1080,
    });
    ensure!(
        port != 0,
        "BROWSER_PROXY_INVALID: proxy port must be nonzero"
    );
    Ok(format!("{scheme}://{host}:{port}"))
}
fn normalize_browser_bypass(value: &str) -> Result<String> {
    ensure!(
        value.len() <= 4096,
        "BROWSER_PROXY_INVALID: proxy exclusion list exceeds limit"
    );
    let mut rules = vec!["<-loopback>".to_owned()];
    for item in value
        .split([',', ';'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        ensure!(
            item.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.*:[]/%<>".contains(c)),
            "BROWSER_PROXY_INVALID: unsupported proxy exclusion rule"
        );
        ensure!(
            !item.contains("//"),
            "BROWSER_PROXY_INVALID: exclusions must be hosts, patterns or IP ranges, not URLs"
        );
        // reqwest NO_PROXY domain rules match both the bare name and its subdomains;
        // Chromium's leading-dot rule matches subdomains only, so emit both.
        let host = item.trim_start_matches('.');
        ensure!(
            !host.is_empty(),
            "BROWSER_PROXY_INVALID: empty proxy exclusion host"
        );
        if !host.contains(['*', '/', '[', '<'])
            && host.parse::<std::net::IpAddr>().is_err()
            && !host
                .chars()
                .all(|c| c.is_ascii_digit() || c == '.' || c == ':')
            && host.chars().any(|c| c.is_ascii_alphabetic())
        {
            for rule in [host.to_owned(), format!("*.{host}")] {
                if !rules.contains(&rule) {
                    rules.push(rule);
                }
            }
        } else if !rules.iter().any(|s| s == item) {
            rules.push(item.to_owned());
        }
    }
    Ok(rules.join(";"))
}
fn resolve_browser_route(
    config: &NetworkConfig,
    env: impl Fn(&str) -> Option<String>,
) -> Result<BrowserRoute> {
    let mode = config.proxy_mode.as_str();
    let (http, https, other, single) = match mode {
        "direct" => (None, None, None, true),
        "manual" => {
            let proxy = normalize_browser_proxy(&config.proxy_url)?;
            (Some(proxy.clone()), Some(proxy.clone()), Some(proxy), true)
        }
        "environment" => {
            let all = proxy_env(&env, &["all_proxy", "ALL_PROXY"])
                .map(|s| normalize_browser_proxy(&s))
                .transpose()?;
            let http_names: &[&str] = if env("REQUEST_METHOD").is_some() {
                &["http_proxy"]
            } else {
                &["http_proxy", "HTTP_PROXY"]
            };
            let http = proxy_env(&env, http_names)
                .map(|s| normalize_browser_proxy(&s))
                .transpose()?
                .or_else(|| all.clone());
            let https = proxy_env(&env, &["https_proxy", "HTTPS_PROXY"])
                .map(|s| normalize_browser_proxy(&s))
                .transpose()?
                .or_else(|| all.clone());
            let other = all.or_else(|| https.clone()).or_else(|| http.clone());
            (http, https, other, false)
        }
        _ => bail!("BROWSER_PROXY_INVALID: unsupported network proxy mode"),
    };
    let proxy_server = if http.is_none() && https.is_none() && other.is_none() {
        None
    } else if single {
        http.clone()
    } else {
        Some(format!(
            "http={};https={};socks={}",
            http.as_deref().unwrap_or("direct://"),
            https.as_deref().unwrap_or("direct://"),
            other.as_deref().unwrap_or("direct://")
        ))
    };
    let bypass = if proxy_server.is_some() {
        let bypass = if !config.no_proxy.is_empty() {
            config.no_proxy.clone()
        } else if mode == "environment" {
            proxy_env(&env, &["no_proxy", "NO_PROXY"]).unwrap_or_default()
        } else {
            String::new()
        };
        normalize_browser_bypass(&bypass)?
    } else {
        String::new()
    };
    Ok(BrowserRoute {
        mode: mode.to_owned(),
        http,
        https,
        other,
        proxy_server,
        bypass,
    })
}
fn response_for(value: &Value, id: u64, method: &str) -> Option<Result<Value>> {
    if value["id"].as_u64() != Some(id) {
        return None;
    }
    if value.get("error").is_some() {
        // Never echo arbitrary browser-origin error text or DOM content as diagnostics.
        Some(Err(anyhow!(
            "CDP {} failed (code {})",
            method,
            value["error"]["code"]
        )))
    } else {
        Some(
            value
                .get("result")
                .cloned()
                .context("CDP response omitted result"),
        )
    }
}
fn output(value: Value) -> Result<ComputerOutput> {
    Ok(ComputerOutput {
        text: serde_json::to_string(&value)?,
        image: None,
    })
}
fn executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}
fn browser_binary() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("CELOOM_BROWSER_PATH") {
        let p = PathBuf::from(path);
        return (p.is_absolute() && executable(&p)).then_some(p);
    }
    let paths = std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into());
    for name in [
        "google-chrome-stable",
        "google-chrome",
        "chromium",
        "chromium-browser",
    ] {
        for dir in std::env::split_paths(&paths) {
            let p = dir.join(name);
            if dir.is_absolute() && executable(&p) {
                return Some(p);
            }
        }
    }
    let p = PathBuf::from("/opt/google/chrome/chrome");
    executable(&p).then_some(p)
}
fn validate_url(value: &str) -> Result<()> {
    ensure!(
        value.len() <= 8192 && !value.contains('\0'),
        "Invalid browser URL"
    );
    let url = url::Url::parse(value).context("Invalid browser URL")?;
    ensure!(
        url.username().is_empty() && url.password().is_none(),
        "Browser URLs must not contain credentials"
    );
    match url.scheme() {
        "https" | "http" => ensure!(url.host_str().is_some(), "URL requires host"),
        "about" => ensure!(value == "about:blank", "Only about:blank is supported"),
        // Explicit user-supplied local URLs; never generated from a host browser profile.
        "file" => {
            ensure!(
                url.host_str()
                    .is_none_or(|h| h.is_empty() || h == "localhost"),
                "Remote file hosts are unsupported"
            );
            url.to_file_path()
                .map_err(|_| anyhow!("Invalid local file URL"))?;
        }
        _ => bail!("Browser supports http(s), explicit file URLs, and about:blank only"),
    }
    Ok(())
}
fn endpoint_from_active_port(text: &str) -> Result<String> {
    let mut lines = text.lines();
    let port: u16 = lines
        .next()
        .context("Missing CDP port")?
        .parse()
        .context("Invalid CDP port")?;
    ensure!(port != 0, "Invalid CDP port");
    let path = lines.next().context("Missing CDP browser path")?;
    ensure!(
        path.starts_with("/devtools/browser/")
            && path.len() < 256
            && path
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"/-_".contains(&c)),
        "Invalid CDP browser path"
    );
    Ok(format!("ws://127.0.0.1:{port}{path}"))
}
async fn close_slot_handoff(slot: &BrowserSlot) -> Option<String> {
    let mut h = slot.handoff.lock().await;
    if let Some(mut win) = h.take() {
        if let Some(pid) = win.child.id() {
            let _ = nix::sys::signal::killpg(
                nix::unistd::Pid::from_raw(pid as i32),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
        let _ = win.child.start_kill();
        let _ = timeout(Duration::from_secs(2), win.child.wait()).await;
        Some(win.url)
    } else {
        None
    }
}
async fn profile_for(data_dir: &Path, owner: &str) -> Result<PathBuf> {
    let root = data_dir.join("computer").join("profiles");
    tokio::fs::create_dir_all(&root).await?;
    let safe_owner: String = owner
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .take(64)
        .collect();
    let profile = root.join(if safe_owner.is_empty() { "default" } else { &safe_owner });
    tokio::fs::create_dir_all(&profile).await?;
    use std::os::unix::fs::PermissionsExt;
    tokio::fs::set_permissions(&profile, std::fs::Permissions::from_mode(0o700)).await?;
    Ok(tokio::fs::canonicalize(profile).await?)
}
async fn launch(
    data_dir: &Path,
    owner: &str,
    route: &BrowserRoute,
    cancel: &CancellationToken,
) -> Result<BrowserSession> {
    let binary = browser_binary().context(
        "BROWSER_UNAVAILABLE: install Chromium/Chrome or set absolute CELOOM_BROWSER_PATH",
    )?;
    let profile = profile_for(data_dir, owner).await?;
    for lock_file in ["DevToolsActivePort", "SingletonLock", "SingletonSocket", "SingletonCookie"] {
        let _ = tokio::fs::remove_file(profile.join(lock_file)).await;
    }
    // Never silently disable Chromium's sandbox. This operator-only escape is restricted to root.
    let no_sandbox = crate::privilege::uid() == 0
        && std::env::var("CELOOM_BROWSER_ALLOW_NO_SANDBOX").as_deref() == Ok("1");
    let mut command = Command::new(binary);
    command.env_clear();
    for name in [
        "HOME",
        "PATH",
        "LANG",
        "LC_ALL",
        "TZ",
        "XDG_RUNTIME_DIR",
        "TMPDIR",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command
        .args([
            "--headless",
            "--remote-debugging-address=127.0.0.1",
            "--remote-debugging-port=0",
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-background-networking",
            "--disable-sync",
            "--disable-extensions",
            "--disable-blink-features=AutomationControlled",
            "--lang=zh-CN",
            "--user-agent=Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
            "--password-store=basic",
            "--window-size=1280,800",
        ])
        .args(route.flags())
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg("about:blank")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .process_group(0);
    if no_sandbox {
        command.arg("--no-sandbox");
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(_) => {
            remove_profile(&profile).await;
            bail!("Cannot start private headless browser");
        }
    };
    let connect: Result<Socket> = async {
        let started = Instant::now();
        let endpoint = loop {
            ensure!(!cancel.is_cancelled(), "Browser startup cancelled");
            ensure!(child.try_wait()?.is_none(), "Private browser exited during startup (sandbox/dependencies may be unavailable)");
            if let Ok(text) = tokio::fs::read_to_string(profile.join("DevToolsActivePort")).await {
                break endpoint_from_active_port(&text)?;
            }
            ensure!(started.elapsed() < Duration::from_secs(12), "Private browser CDP startup timed out");
            tokio::select! { _=cancel.cancelled()=>bail!("Browser startup cancelled"), _=tokio::time::sleep(Duration::from_millis(40))=>{} }
        };
        let config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
            .max_message_size(Some(MAX_CDP_MESSAGE)).max_frame_size(Some(MAX_CDP_MESSAGE));
        let (socket, _) = tokio::select! {
            _=cancel.cancelled()=>bail!("Browser startup cancelled"),
            result=timeout(RPC_TIMEOUT,tokio_tungstenite::connect_async_with_config(endpoint,Some(config),true))=> result.context("CDP connect timeout")?.map_err(|_|anyhow!("Cannot connect to private browser CDP"))?
        };
        Ok(socket)
    }.await;
    let socket = match connect {
        Ok(socket) => socket,
        Err(error) => {
            if let Some(pid) = child.id() {
                let _ = nix::sys::signal::killpg(
                    nix::unistd::Pid::from_raw(pid as i32),
                    nix::sys::signal::Signal::SIGKILL,
                );
            }
            let _ = child.kill().await;
            remove_profile(&profile).await;
            return Err(error);
        }
    };
    let mut session = BrowserSession {
        owner: owner.to_owned(),
        child,
        profile,
        socket,
        target: String::new(),
        session: String::new(),
        next_id: 1,
        epoch: 1,
        snapshot: None,
        refs: HashMap::new(),
        screenshot_epoch: None,
        screenshot_stamp: None,
        screenshot_at: None,
        healthy: true,
        sandbox: !no_sandbox,
        network_route: route.clone(),
    };
    let init = session.initialize(cancel).await;
    if let Err(error) = init {
        terminate(&mut session).await;
        return Err(error);
    }
    let cookies_file = data_dir.join("computer").join("imported-cookies.json");
    if let Ok(raw) = tokio::fs::read_to_string(&cookies_file).await
        && let Ok(cookies) = serde_json::from_str::<Vec<Value>>(&raw)
        && !cookies.is_empty()
    {
        let _ = session
            .rpc("Network.setCookies", json!({ "cookies": cookies }), cancel)
            .await;
    }
    Ok(session)
}
async fn remove_profile(profile: &Path) {
    // Only a direct, non-symlink browser-UUID child of the private computer directory.
    let valid = profile
        .file_name()
        .and_then(|s| s.to_str())
        .and_then(|s| s.strip_prefix("browser-"))
        .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
        && profile
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|n| n == "computer")
        && tokio::fs::symlink_metadata(profile)
            .await
            .is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink());
    if valid {
        // Keep persistent profile intact; only clean ephemeral browser-UUID dirs if any remain.
        let _ = tokio::fs::remove_dir_all(profile).await;
    }
}
async fn terminate(session: &mut BrowserSession) {
    if let Some(pid) = session.child.id() {
        let _ = nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(pid as i32),
            nix::sys::signal::Signal::SIGKILL,
        );
    }
    let _ = session.child.start_kill();
    let _ = timeout(Duration::from_secs(3), session.child.wait()).await;
    remove_profile(&session.profile).await;
    session.healthy = false;
}
impl BrowserSession {
    fn invalidate(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        self.refs.clear();
        self.snapshot = None;
        self.screenshot_epoch = None;
    }
    fn event(&mut self, value: &Value) {
        if value["sessionId"]
            .as_str()
            .is_some_and(|s| s != self.session)
        {
            return;
        }
        match value["method"].as_str().unwrap_or("") {
            "DOM.documentUpdated"
            | "Runtime.executionContextsCleared"
            | "Runtime.executionContextDestroyed"
            | "Page.navigatedWithinDocument"
            | "Inspector.detached" => self.invalidate(),
            "Page.frameNavigated" if value["params"]["frame"]["parentId"].is_null() => {
                self.invalidate()
            }
            "Target.detachedFromTarget"
                if value["params"]["sessionId"].as_str() == Some(&self.session) =>
            {
                self.invalidate();
                self.healthy = false;
            }
            _ => {}
        }
    }
    async fn rpc(
        &mut self,
        method: &str,
        params: Value,
        cancel: &CancellationToken,
    ) -> Result<Value> {
        ensure!(
            self.healthy,
            "BROWSER_DISCONNECTED: use open to create a new private browser"
        );
        let id = self.next_id;
        self.next_id += 1;
        let mut request = json!({"id":id,"method":method,"params":params});
        if !self.session.is_empty()
            && !method.starts_with("Target.")
            && !method.starts_with("Browser.")
        {
            request["sessionId"] = json!(self.session);
        }
        let result = tokio::select! {
            _=cancel.cancelled()=>Err(anyhow!("CANCELLED: CDP operation outcome unknown")),
            result=timeout(RPC_TIMEOUT, async {
                self.socket.send(Message::Text(request.to_string().into())).await.map_err(|_|anyhow!("CDP send failed; outcome unknown"))?;
                loop {
                    let message = self.socket.next().await.context("CDP disconnected; outcome unknown")?.map_err(|_|anyhow!("CDP connection failed; outcome unknown"))?;
                    match message {
                        Message::Text(text) => {
                            ensure!(text.len() <= MAX_CDP_MESSAGE,"CDP response exceeds limit");
                            let value: Value = serde_json::from_str(&text).context("Invalid CDP response")?;
                            if let Some(result)=response_for(&value,id,method) { return Ok(result); }
                            self.event(&value);
                        }
                        Message::Ping(data) => { self.socket.send(Message::Pong(data)).await.map_err(|_|anyhow!("CDP ping failed"))?; }
                        Message::Close(_) => bail!("CDP closed; outcome unknown"),
                        _ => {}
                    }
                }
            }) => match result { Ok(r)=>r, Err(_)=>Err(anyhow!("CDP timeout; outcome unknown; not replayed")) }
        };
        match result {
            Ok(protocol_result) => protocol_result,
            Err(error) => {
                self.healthy = false;
                Err(error)
            }
        }
    }
    async fn initialize(&mut self, cancel: &CancellationToken) -> Result<()> {
        let targets = self.rpc("Target.getTargets", json!({}), cancel).await?;
        let target = targets["targetInfos"]
            .as_array()
            .and_then(|items| items.iter().find(|v| v["type"] == "page"))
            .and_then(|v| v["targetId"].as_str())
            .context("Private browser has no page target")?;
        self.target = target.to_owned();
        self.session = self
            .rpc(
                "Target.attachToTarget",
                json!({"targetId":self.target,"flatten":true}),
                cancel,
            )
            .await?["sessionId"]
            .as_str()
            .context("Missing CDP session")?
            .to_owned();
        for method in [
            "Page.enable",
            "Runtime.enable",
            "DOM.enable",
            "Accessibility.enable",
        ] {
            self.rpc(method, json!({}), cancel).await?;
        }
        self.rpc(
            "Emulation.setDeviceMetricsOverride",
            json!({"width":WIDTH,"height":HEIGHT,"deviceScaleFactor":1,"mobile":false}),
            cancel,
        )
        .await?;
        self.rpc(
            "Page.addScriptToEvaluateOnNewDocument",
            json!({"source":INSTALL_REVISION}),
            cancel,
        )
        .await?;
        self.evaluate(INSTALL_REVISION, cancel).await?;
        Ok(())
    }
    async fn evaluate(&mut self, script: &str, cancel: &CancellationToken) -> Result<Value> {
        let reply = self.rpc("Runtime.evaluate",json!({"expression":script,"returnByValue":true,"awaitPromise":false,"timeout":1500}),cancel).await?;
        ensure!(
            reply.get("exceptionDetails").is_none(),
            "Internal browser DOM function failed"
        );
        Ok(reply["result"]["value"].clone())
    }
    async fn on_node(
        &mut self,
        object: &str,
        function: &str,
        arguments: Value,
        cancel: &CancellationToken,
    ) -> Result<Value> {
        let reply = self.rpc("Runtime.callFunctionOn",json!({"objectId":object,"functionDeclaration":function,"arguments":arguments,"returnByValue":true,"awaitPromise":false}),cancel).await?;
        ensure!(
            reply.get("exceptionDetails").is_none(),
            "Browser element function failed"
        );
        Ok(reply["result"]["value"].clone())
    }
    async fn location(&mut self, cancel: &CancellationToken) -> Result<Value> {
        self.evaluate(
            "({url:location.href,title:document.title,ready:document.readyState})",
            cancel,
        )
        .await
    }
    async fn perform(
        &mut self,
        action: &ComputerAction,
        cancel: &CancellationToken,
    ) -> Result<ComputerOutput> {
        match action.action.as_str() {
            "open" => {
                let url = action.url.as_deref().context("open requires url")?;
                validate_url(url)?;
                self.invalidate();
                let navigation = self
                    .rpc("Page.navigate", json!({"url":url}), cancel)
                    .await?;
                ensure!(
                    navigation.get("errorText").is_none(),
                    "BROWSER_NAVIGATION_FAILED"
                );
                let expected_loader = navigation["loaderId"].as_str().map(str::to_owned);
                let start = Instant::now();
                loop {
                    let frame = self.rpc("Page.getFrameTree", json!({}), cancel).await?;
                    let loaded = expected_loader.as_deref().is_none_or(|expected| {
                        frame["frameTree"]["frame"]["loaderId"].as_str() == Some(expected)
                    });
                    // Readiness queries are read-only and may fail transiently while the old
                    // execution context is destroyed. No navigation/input is ever retried.
                    if let Ok(location) = self.location(cancel).await {
                        let ready = location["ready"].as_str().unwrap_or("");
                        let cur_url = location["url"].as_str().unwrap_or("");
                        if (loaded && (ready == "complete" || ready == "interactive"))
                            || (start.elapsed() >= Duration::from_secs(7) && !cur_url.is_empty() && cur_url != "about:blank")
                        {
                            break;
                        }
                    }
                    ensure!(self.healthy, "Browser disconnected during navigation");
                    ensure!(
                        start.elapsed() < Duration::from_secs(10),
                        "Navigation readiness timed out; inspect again rather than repeat an uncertain action"
                    );
                    tokio::select! { _=cancel.cancelled()=>bail!("Cancelled navigation"), _=tokio::time::sleep(Duration::from_millis(35))=>{} }
                }
                self.observe(action.max_nodes, action.max_depth, cancel).await
            }
            "observe" | "resume_headless" => self.observe(action.max_nodes, action.max_depth, cancel).await,
            "screenshot" => self.screenshot(cancel).await,
            "click" => {
                let (x, y, epoch) = if let Some(id) = action.ref_id.as_deref() {
                    let (object, _) = self.resolve_ref(id, cancel).await?;
                    self.on_node(&object,"function(){this.scrollIntoView({block:'center',inline:'center',behavior:'instant'});return true}",json!([]),cancel).await?;
                    // Scrolling may trigger lazy/virtualized DOM recycling. Re-resolve the
                    // exact backend node and semantic fingerprint before calculating a hit.
                    let (object, reference) = self.resolve_ref(id, cancel).await?;
                    let meta = self.on_node(&object, METADATA, json!([]), cancel).await?;
                    ensure!(
                        meta["visible"] == true && meta["disabled"] != true,
                        "ELEMENT_NOT_INTERACTABLE"
                    );
                    let rect = &meta["rect"];
                    let x = number(rect, "x")? + number(rect, "width")? / 2.0;
                    let y = number(rect, "y")? + number(rect, "height")? / 2.0;
                    let hit = self.on_node(&object,"function(x,y){let e=document.elementFromPoint(x,y);while(e&&e.shadowRoot){let n=e.shadowRoot.elementFromPoint(x,y);if(!n||n===e)break;e=n}return e===this||this.contains(e)}",json!([{"value":x},{"value":y}]),cancel).await?;
                    ensure!(
                        hit == true,
                        "ELEMENT_OCCLUDED: observe again; no click sent"
                    );
                    (x, y, reference.epoch)
                } else {
                    ensure!(
                        self.screenshot_epoch == Some(self.epoch)
                            && self
                                .screenshot_at
                                .is_some_and(|t| t.elapsed() < Duration::from_secs(10)),
                        "SCREENSHOT_REQUIRED: coordinates require a fresh screenshot captured within 10 seconds"
                    );
                    let current = self.evaluate(SCREENSHOT_STAMP, cancel).await?;
                    ensure!(
                        self.screenshot_stamp.as_ref() == Some(&current)
                            && self.screenshot_epoch == Some(self.epoch),
                        "STALE_SCREENSHOT: DOM or viewport changed; capture again before coordinate click"
                    );
                    (
                        action.x.context("click requires ref_id or x/y")? as f64,
                        action.y.context("click requires ref_id or x/y")? as f64,
                        self.epoch,
                    )
                };
                ensure!(epoch == self.epoch, "STALE_REF: document changed");
                check_point(x, y)?;
                let button = action.button.as_deref().unwrap_or("left");
                ensure!(
                    matches!(button, "left" | "middle" | "right"),
                    "Invalid browser button"
                );
                // Consume coordinates before dispatch: no replay using an earlier screenshot.
                self.screenshot_epoch = None;
                self.rpc(
                    "Input.dispatchMouseEvent",
                    json!({"type":"mousePressed","x":x,"y":y,"button":button,"clickCount":1}),
                    cancel,
                )
                .await?;
                self.rpc(
                    "Input.dispatchMouseEvent",
                    json!({"type":"mouseReleased","x":x,"y":y,"button":button,"clickCount":1}),
                    cancel,
                )
                .await?;
                self.action_reply("click", cancel).await
            }
            "fill" => {
                let id = action.ref_id.as_deref().context("fill requires ref_id")?;
                let text = bounded_input(action.text.as_deref().context("fill requires text")?)?;
                let (object, reference) = self.resolve_ref(id, cancel).await?;
                let prepared = self
                    .on_node(&object, PREPARE_FILL, json!([]), cancel)
                    .await?;
                ensure!(
                    prepared == true,
                    "ELEMENT_NOT_EDITABLE: fill supports enabled input/textarea/contenteditable"
                );
                ensure!(reference.epoch == self.epoch, "STALE_REF: document changed");
                self.screenshot_epoch = None;
                self.dispatch_key("Ctrl+A", cancel).await?;
                if text.is_empty() {
                    self.dispatch_key("Backspace", cancel).await?;
                } else {
                    self.rpc("Input.insertText", json!({"text":text}), cancel)
                        .await?;
                }
                // Finish a real editing interaction rather than setAttribute(value).
                let value = self.on_node(&object,"function(){this.dispatchEvent(new Event('change',{bubbles:true}));return this.isContentEditable?this.textContent:this.value}",json!([]),cancel).await?;
                ensure!(
                    value.as_str() == Some(text),
                    "FILL_VERIFICATION_FAILED: application value differs; not replayed"
                );
                self.action_reply("fill", cancel).await
            }
            "type" => {
                let text = bounded_input(action.text.as_deref().context("type requires text")?)?;
                if let Some(id) = action.ref_id.as_deref() {
                    let (object, reference) = self.resolve_ref(id, cancel).await?;
                    self.on_node(
                        &object,
                        "function(){this.focus();return true}",
                        json!([]),
                        cancel,
                    )
                    .await?;
                    ensure!(reference.epoch == self.epoch, "STALE_REF: document changed");
                }
                let active = self.evaluate("(()=>{let e=document.activeElement;while(e&&e.shadowRoot&&e.shadowRoot.activeElement)e=e.shadowRoot.activeElement;return !!e&&!e.disabled&&!e.readOnly&&(e.isContentEditable||/^(INPUT|TEXTAREA)$/.test(e.tagName))})()",cancel).await?;
                ensure!(
                    active == true,
                    "NO_EDITABLE_FOCUS: choose an editable ref first"
                );
                self.screenshot_epoch = None;
                self.rpc("Input.insertText", json!({"text":text}), cancel)
                    .await?;
                self.action_reply("type", cancel).await
            }
            "key" => {
                let key = action.key.as_deref().context("key requires key")?;
                self.screenshot_epoch = None;
                self.dispatch_key(key, cancel).await?;
                self.action_reply("key", cancel).await
            }
            "scroll" => {
                let delta = action.delta.context("scroll requires delta")?;
                ensure!(
                    delta != 0 && delta.unsigned_abs() <= 4000,
                    "Scroll delta must be nonzero and within 4000 CSS pixels"
                );
                let x = action.x.unwrap_or((WIDTH / 2) as i32) as f64;
                let y = action.y.unwrap_or((HEIGHT / 2) as i32) as f64;
                check_point(x, y)?;
                self.screenshot_epoch = None;
                self.rpc(
                    "Input.dispatchMouseEvent",
                    json!({"type":"mouseWheel","x":x,"y":y,"deltaX":0,"deltaY":delta}),
                    cancel,
                )
                .await?;
                self.action_reply("scroll", cancel).await
            }
            "hover" => {
                let (x, y) = if let Some(id) = action.ref_id.as_deref() {
                    let (object, _) = self.resolve_ref(id, cancel).await?;
                    let meta = self.on_node(&object, METADATA, json!([]), cancel).await?;
                    let rect = &meta["rect"];
                    (
                        number(rect, "x")? + number(rect, "width")? / 2.0,
                        number(rect, "y")? + number(rect, "height")? / 2.0,
                    )
                } else {
                    let x = action.x.context("hover requires ref_id or x/y")? as f64;
                    let y = action.y.context("hover requires ref_id or x/y")? as f64;
                    check_point(x, y)?;
                    (x, y)
                };
                self.rpc(
                    "Input.dispatchMouseEvent",
                    json!({"type":"mouseMoved","x":x,"y":y}),
                    cancel,
                )
                .await?;
                self.action_reply("hover", cancel).await
            }
            "back" => {
                self.invalidate();
                self.rpc(
                    "Runtime.evaluate",
                    json!({"expression":"history.back()","returnByValue":true}),
                    cancel,
                )
                .await?;
                tokio::select! { _ = cancel.cancelled() => bail!("Cancelled"), _ = tokio::time::sleep(Duration::from_millis(500)) => {} }
                self.observe(None, None, cancel).await
            }
            "wait" => {
                let ms = action.delta.unwrap_or(1500).clamp(100, 10000) as u64;
                if let Some(selector) = action.text.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                    let expr = format!(
                        "Boolean(document.querySelector({}))",
                        serde_json::to_string(selector)?
                    );
                    let start = Instant::now();
                    loop {
                        let res = self
                            .rpc(
                                "Runtime.evaluate",
                                json!({"expression": expr, "returnByValue": true}),
                                cancel,
                            )
                            .await?;
                        if res["result"]["value"].as_bool() == Some(true)
                            || start.elapsed() >= Duration::from_millis(ms)
                        {
                            break;
                        }
                        tokio::select! { _ = cancel.cancelled() => bail!("Cancelled"), _ = tokio::time::sleep(Duration::from_millis(150)) => {} }
                    }
                } else {
                    tokio::select! { _ = cancel.cancelled() => bail!("Cancelled"), _ = tokio::time::sleep(Duration::from_millis(ms)) => {} }
                }
                self.observe(None, None, cancel).await
            }
            "eval" => {
                let expr = action
                    .text
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .context("eval requires JS expression in text")?;
                let res = match self
                    .rpc(
                        "Runtime.evaluate",
                        json!({"expression": expr, "returnByValue": true, "awaitPromise": true}),
                        cancel,
                    )
                    .await
                {
                    Ok(v) => v,
                    Err(err) => {
                        let msg = err.to_string();
                        if msg.contains("-32000") || msg.contains("Execution context") {
                            tokio::select! { _ = cancel.cancelled() => bail!("Cancelled"), _ = tokio::time::sleep(Duration::from_millis(600)) => {} }
                            let location = self.location(cancel).await.unwrap_or(Value::Null);
                            return output(json!({
                                "ok": true,
                                "action": "eval",
                                "target": "browser",
                                "navigated": true,
                                "page": location,
                                "note": "JS triggered page navigation (execution context changed); call observe to inspect the new page."
                            }));
                        }
                        return Err(err);
                    }
                };
                if let Some(desc) = res["exceptionDetails"]["exception"]["description"].as_str() {
                    bail!("JS eval error: {}", clip(desc, 300));
                }
                let val = &res["result"]["value"];
                let text = if let Some(s) = val.as_str() {
                    clip(s, 12000)
                } else {
                    clip(&serde_json::to_string(val).unwrap_or_default(), 12000)
                };
                output(json!({"ok":true,"action":"eval","target":"browser","result":text}))
            }
            "cookies" => {
                let res = self.rpc("Network.getCookies", json!({}), cancel).await?;
                let cookies = res["cookies"].as_array().cloned().unwrap_or_default();
                let mut by_domain: std::collections::BTreeMap<String, Vec<String>> =
                    std::collections::BTreeMap::new();
                for c in &cookies {
                    let domain = c["domain"].as_str().unwrap_or("").to_string();
                    let name = c["name"].as_str().unwrap_or("").to_string();
                    if !domain.is_empty() && !name.is_empty() {
                        by_domain.entry(domain).or_default().push(name);
                    }
                }
                output(json!({
                    "ok": true,
                    "action": "cookies",
                    "count": cookies.len(),
                    "domains": by_domain,
                    "note": "Cookie values are withheld for security; presence confirms login state."
                }))
            }
            _ => bail!("Unsupported browser action"),
        }
    }
    async fn action_reply(
        &mut self,
        action: &str,
        cancel: &CancellationToken,
    ) -> Result<ComputerOutput> {
        let location = self.location(cancel).await?;
        output(
            json!({"ok":true,"action":action,"target":"browser","target_id":self.target,"document_epoch":self.epoch,"page":location,"outcome":"applied","note":"Input dispatched once; inspect DOM/screenshot to verify application-level completion."}),
        )
    }
    async fn dispatch_key(&mut self, value: &str, cancel: &CancellationToken) -> Result<()> {
        let key = parse_key(value)?;
        let mut params = json!({"type":"keyDown","key":key.key,"code":key.code,"windowsVirtualKeyCode":key.virtual_code,"nativeVirtualKeyCode":key.virtual_code,"modifiers":key.modifiers});
        if let Some(text) = key.text {
            params["text"] = json!(text);
            params["unmodifiedText"] = json!(text);
        }
        self.rpc("Input.dispatchKeyEvent", params.clone(), cancel)
            .await?;
        params["type"] = json!("keyUp");
        params.as_object_mut().unwrap().remove("text");
        params.as_object_mut().unwrap().remove("unmodifiedText");
        self.rpc("Input.dispatchKeyEvent", params, cancel).await?;
        Ok(())
    }
    async fn observe(
        &mut self,
        max_nodes: Option<u32>,
        max_depth: Option<u32>,
        cancel: &CancellationToken,
    ) -> Result<ComputerOutput> {
        let node_cap: usize = match max_nodes {
            Some(0) => 2000,
            Some(n) => (n as usize).clamp(1, 2000),
            None => 250,
        };
        let depth_cap: usize = match max_depth {
            Some(0) => 64,
            Some(d) => (d as usize).clamp(1, 64),
            None => 12,
        };
        self.refs.clear();
        self.snapshot = None;
        self.rpc("DOM.getDocument", json!({"depth":0,"pierce":true}), cancel)
            .await?;
        let ax = self
            .rpc("Accessibility.getFullAXTree", json!({}), cancel)
            .await
            .unwrap_or_else(|_| json!({"nodes":[]}));
        ensure!(
            self.healthy,
            "CDP disconnected during accessibility snapshot"
        );
        let mut ax_by_backend = HashMap::new();
        if let Some(nodes) = ax["nodes"].as_array() {
            for node in nodes {
                if let Some(id) = node["backendDOMNodeId"].as_i64() {
                    ax_by_backend.insert(id, node.clone());
                }
            }
        }
        let snapshot = uuid::Uuid::new_v4().to_string();
        let collect_script = format!(
            "(()=>{{const CAP={node_cap},MAX_D={depth_cap};let out=[];function scan(root,d){{if(d>MAX_D||out.length>=CAP)return;for(const e of root.children||[]){{if(out.length>=CAP)return;const s=getComputedStyle(e),r=e.getBoundingClientRect();if(s.display!=='none'&&s.visibility==='visible'&&r.width>0&&r.height>0&&!(e.tagName==='INPUT'&&e.type==='hidden')){{if(e.matches('a,button,input,textarea,select,summary,[role],[contenteditable],[tabindex],label,h1,h2,h3,h4')||e.onclick)out.push(e)}}if(e.shadowRoot)scan(e.shadowRoot,d+1);scan(e,d+1)}}}}scan(document.documentElement||document,0);return out}})()"
        );
        let reply = self.rpc("Runtime.evaluate",json!({"expression":collect_script,"objectGroup":"celoom-observe","returnByValue":false}),cancel).await?;
        ensure!(
            reply.get("exceptionDetails").is_none(),
            "DOM snapshot failed"
        );
        let object = reply["result"]["objectId"]
            .as_str()
            .context("DOM snapshot has no array")?
            .to_owned();
        let properties = self
            .rpc(
                "Runtime.getProperties",
                json!({"objectId":object,"ownProperties":true}),
                cancel,
            )
            .await?;
        let epoch = self.epoch;
        let mut nodes = Vec::new();
        if let Some(properties) = properties["result"].as_array() {
            for property in properties {
                if property["name"]
                    .as_str()
                    .is_none_or(|s| s.parse::<usize>().is_err())
                {
                    continue;
                }
                let Some(object) = property["value"]["objectId"].as_str() else {
                    continue;
                };
                let description = self
                    .rpc(
                        "DOM.describeNode",
                        json!({"objectId":object,"depth":0}),
                        cancel,
                    )
                    .await?;
                let Some(backend) = description["node"]["backendNodeId"].as_i64() else {
                    continue;
                };
                let mut meta = self.on_node(object, METADATA, json!([]), cancel).await?;
                if meta["visible"] != true {
                    continue;
                }
                let ax = ax_by_backend.get(&backend);
                let ax_identity = ax.map(|n| {
                    (
                        n["role"]["value"].as_str().unwrap_or("").to_owned(),
                        n["name"]["value"].as_str().unwrap_or("").to_owned(),
                    )
                });
                if let Some((role, name)) = &ax_identity {
                    meta["role"] = json!(clip(role, 80));
                    meta["name"] = json!(clip(name, 160));
                }
                let id = format!("{}:e{}", snapshot, nodes.len() + 1);
                self.refs.insert(
                    id.clone(),
                    ElementRef {
                        backend,
                        epoch,
                        fingerprint: fingerprint(&meta),
                        ax_identity,
                    },
                );
                meta["ref_id"] = json!(id);
                nodes.push(meta);
                if nodes.len() >= node_cap {
                    break;
                }
            }
        }
        let _ = self
            .rpc(
                "Runtime.releaseObjectGroup",
                json!({"objectGroup":"celoom-observe"}),
                cancel,
            )
            .await;
        ensure!(
            self.epoch == epoch,
            "STALE_SNAPSHOT: document changed during observation; observe again"
        );
        self.snapshot = Some(snapshot.clone());
        let mut page = self.evaluate("({url:location.href,title:document.title,text:(document.body?.innerText||'').slice(0,4500),frames:document.querySelectorAll('iframe,frame').length})",cancel).await?;
        for (field, limit) in [("url", 2048), ("title", 160), ("text", 2500)] {
            if let Some(value) = page[field].as_str() {
                page[field] = json!(clip(value, limit));
            }
        }
        ensure!(
            self.epoch == epoch,
            "STALE_SNAPSHOT: document changed during observation"
        );
        let total_collected = nodes.len();
        let mut response = json!({"ok":true,"target":"browser","target_id":self.target,"snapshot_id":snapshot,"document_epoch":epoch,"page":page,"nodes":nodes,"total_collected":total_collected,"max_nodes":node_cap,"max_depth":depth_cap,"refs_expire":"navigation, document replacement, next observe, or semantic element change","frame_scope":"main document and open shadow roots only; iframe contents are not represented","untrusted_content":true,"outcome":"observed"});
        // Preserve complete JSON before the engine's text admission limit; never return
        // a truncated JSON suffix or references for nodes the model did not receive.
        while response.to_string().len() > 48_000 {
            let removed = response["nodes"].as_array_mut().unwrap().pop();
            if let Some(removed) = removed {
                if let Some(id) = removed["ref_id"].as_str() {
                    self.refs.remove(id);
                }
                response["nodes_truncated"] = json!(true);
            } else {
                break;
            }
        }
        output(response)
    }
    async fn resolve_ref(
        &mut self,
        id: &str,
        cancel: &CancellationToken,
    ) -> Result<(String, ElementRef)> {
        let reference = self
            .refs
            .get(id)
            .cloned()
            .context("STALE_REF: observe again; unknown or expired ref")?;
        ensure!(reference.epoch == self.epoch, "STALE_REF: document changed");
        self.rpc(
            "Runtime.releaseObjectGroup",
            json!({"objectGroup":"celoom-action"}),
            cancel,
        )
        .await?;
        let reply = self
            .rpc(
                "DOM.resolveNode",
                json!({"backendNodeId":reference.backend,"objectGroup":"celoom-action"}),
                cancel,
            )
            .await
            .map_err(|e| anyhow!("STALE_REF: cannot resolve original element ({e})"))?;
        let object = reply["object"]["objectId"]
            .as_str()
            .context("STALE_REF: element no longer exists")?
            .to_owned();
        let mut meta = self.on_node(&object, METADATA, json!([]), cancel).await?;
        ensure!(
            meta["connected"] == true && meta["main_document"] == true && meta["visible"] == true,
            "STALE_REF: original element disconnected, hidden, or moved"
        );
        if let Some(expected) = &reference.ax_identity {
            let ax = self
                .rpc(
                    "Accessibility.getPartialAXTree",
                    json!({"backendNodeId":reference.backend,"fetchRelatives":false}),
                    cancel,
                )
                .await?;
            let current = ax["nodes"]
                .as_array()
                .and_then(|n| {
                    n.iter()
                        .find(|n| n["backendDOMNodeId"].as_i64() == Some(reference.backend))
                })
                .context("STALE_REF: accessibility identity missing")?;
            let actual = (
                current["role"]["value"].as_str().unwrap_or(""),
                current["name"]["value"].as_str().unwrap_or(""),
            );
            ensure!(
                actual.0 == expected.0 && actual.1 == expected.1,
                "STALE_REF: accessible role/name changed"
            );
            meta["role"] = json!(clip(actual.0, 80));
            meta["name"] = json!(clip(actual.1, 160));
        }
        ensure!(
            fingerprint(&meta) == reference.fingerprint && reference.epoch == self.epoch,
            "STALE_REF: element identity changed; observe again"
        );
        ensure!(meta["disabled"] != true, "ELEMENT_DISABLED");
        Ok((object, reference))
    }
    async fn screenshot(&mut self, cancel: &CancellationToken) -> Result<ComputerOutput> {
        let before = self.evaluate(SCREENSHOT_STAMP, cancel).await?;
        let epoch = self.epoch;
        let reply = self
            .rpc(
                "Page.captureScreenshot",
                json!({"format":"png","fromSurface":true,"captureBeyondViewport":false}),
                cancel,
            )
            .await?;
        let data = reply["data"].as_str().context("Screenshot has no PNG")?;
        ensure!(
            data.len() <= 12 * 1024 * 1024,
            "Screenshot exceeds image budget"
        );
        let png = STANDARD
            .decode(data)
            .context("Invalid screenshot encoding")?;
        let reader =
            image::ImageReader::with_format(std::io::Cursor::new(&png), image::ImageFormat::Png);
        let (width, height) = reader.into_dimensions().context("Invalid screenshot PNG")?;
        ensure!(
            width > 0 && height > 0 && width <= WIDTH && height <= HEIGHT,
            "Screenshot geometry exceeds fixed viewport"
        );
        let page = self.evaluate("({url:location.href,scroll_x:scrollX,scroll_y:scrollY,dpr:devicePixelRatio,width:innerWidth,height:innerHeight})",cancel).await?;
        let after = self.evaluate(SCREENSHOT_STAMP, cancel).await?;
        let stable = epoch == self.epoch && before == after;
        self.screenshot_epoch = stable.then_some(self.epoch);
        self.screenshot_stamp = stable.then_some(after);
        self.screenshot_at = stable.then(Instant::now);
        Ok(ComputerOutput{text:json!({"ok":true,"target":"browser","target_id":self.target,"document_epoch":self.epoch,"coordinate_click_ready":stable,"image_attached":true,"mime":"image/png","width":width,"height":height,"page":page,"coordinate_space":"viewport CSS pixels; deviceScaleFactor=1","outcome":"observed"}).to_string(),image:Some(ComputerImage{png,width,height})})
    }
}
fn number(v: &Value, name: &str) -> Result<f64> {
    v[name]
        .as_f64()
        .filter(|n| n.is_finite())
        .context("Invalid element rectangle")
}
fn check_point(x: f64, y: f64) -> Result<()> {
    ensure!(
        x.is_finite()
            && y.is_finite()
            && x >= 0.0
            && y >= 0.0
            && x < WIDTH as f64
            && y < HEIGHT as f64,
        "Coordinate outside private browser viewport"
    );
    Ok(())
}
fn clip(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}
fn bounded_input(value: &str) -> Result<&str> {
    ensure!(
        value.len() <= 32_000 && !value.contains('\0'),
        "Browser text exceeds 32000 bytes or contains NUL"
    );
    Ok(value)
}
fn fingerprint(meta: &Value) -> Value {
    json!({"tag":meta["tag"],"input_type":meta["input_type"],"label":meta["label"],"role":meta["role"],"name":meta["name"],"identity_text":meta["identity_text"]})
}
struct KeySpec {
    key: String,
    code: String,
    virtual_code: u32,
    modifiers: u32,
    text: Option<String>,
}
fn parse_key(value: &str) -> Result<KeySpec> {
    ensure!(!value.is_empty() && value.len() <= 80, "Invalid key");
    // A final literal '+' is a key, not an empty modifier: '+' and 'Ctrl++'.
    let (prefix, name) = if value == "+" {
        ("", "+")
    } else if let Some(prefix) = value.strip_suffix("++") {
        ensure!(!prefix.is_empty(), "Invalid key");
        (prefix, "+")
    } else {
        value.rsplit_once('+').unwrap_or(("", value))
    };
    let mut modifiers = 0;
    if !prefix.is_empty() {
        for modifier in prefix.split('+') {
            modifiers |= match modifier.to_ascii_lowercase().as_str() {
                "alt" => 1,
                "ctrl" | "control" => 2,
                "meta" | "super" => 4,
                "shift" => 8,
                _ => bail!("Unsupported key modifier"),
            };
        }
    } else {
        ensure!(value == name, "Invalid key modifier");
    }
    let name = name.to_ascii_lowercase();
    let named = match name.as_str() {
        "enter" | "return" => Some(("Enter", "Enter", 13, Some("\r"))),
        "tab" => Some(("Tab", "Tab", 9, None)),
        "escape" | "esc" => Some(("Escape", "Escape", 27, None)),
        "backspace" => Some(("Backspace", "Backspace", 8, None)),
        "delete" | "del" => Some(("Delete", "Delete", 46, None)),
        "insert" | "ins" => Some(("Insert", "Insert", 45, None)),
        "arrowleft" | "left" => Some(("ArrowLeft", "ArrowLeft", 37, None)),
        "arrowup" | "up" => Some(("ArrowUp", "ArrowUp", 38, None)),
        "arrowright" | "right" => Some(("ArrowRight", "ArrowRight", 39, None)),
        "arrowdown" | "down" => Some(("ArrowDown", "ArrowDown", 40, None)),
        "home" => Some(("Home", "Home", 36, None)),
        "end" => Some(("End", "End", 35, None)),
        "pageup" | "pgup" => Some(("PageUp", "PageUp", 33, None)),
        "pagedown" | "pgdn" => Some(("PageDown", "PageDown", 34, None)),
        "space" | "spacebar" | " " => Some((" ", "Space", 32, Some(" "))),
        _ => None,
    };
    let (key, code, virtual_code, text) = if let Some((key, code, vk, text)) = named {
        (key.to_owned(), code.to_owned(), vk, text.map(str::to_owned))
    } else {
        let name = match name.as_str() {
            "plus" => "+",
            "minus" => "-",
            "equal" | "equals" => "=",
            "comma" => ",",
            "period" => ".",
            "slash" => "/",
            "backslash" => "\\",
            "semicolon" => ";",
            "quote" | "apostrophe" => "'",
            "backquote" | "grave" => "`",
            "bracketleft" | "leftbracket" => "[",
            "bracketright" | "rightbracket" => "]",
            _ => name.as_str(),
        };
        ensure!(
            name.len() == 1 && name.is_ascii(),
            "Unsupported key: use ASCII letters, digits, punctuation or named navigation/editing keys; use type for text"
        );
        let c = name.as_bytes()[0];
        let (key, code, vk) = if c.is_ascii_alphabetic() {
            // Key names are case-insensitive: bare 'B' means key='b', not Shift+B.
            // Only an explicit Shift modifier uppercases an alphabetic event.key.
            let key = if modifiers & 8 != 0 {
                c.to_ascii_uppercase()
            } else {
                c
            };
            (
                key,
                format!("Key{}", c.to_ascii_uppercase() as char),
                c.to_ascii_uppercase() as u32,
            )
        } else {
            // Deterministic US physical-key mapping. A shifted symbol (e.g. '+')
            // necessarily carries Shift; Shift+an unshifted key selects its symbol.
            let (plain, shifted, code, vk) = match c {
                b'0' | b')' => (b'0', b')', "Digit0", 48),
                b'1' | b'!' => (b'1', b'!', "Digit1", 49),
                b'2' | b'@' => (b'2', b'@', "Digit2", 50),
                b'3' | b'#' => (b'3', b'#', "Digit3", 51),
                b'4' | b'$' => (b'4', b'$', "Digit4", 52),
                b'5' | b'%' => (b'5', b'%', "Digit5", 53),
                b'6' | b'^' => (b'6', b'^', "Digit6", 54),
                b'7' | b'&' => (b'7', b'&', "Digit7", 55),
                b'8' | b'*' => (b'8', b'*', "Digit8", 56),
                b'9' | b'(' => (b'9', b'(', "Digit9", 57),
                b'-' | b'_' => (b'-', b'_', "Minus", 189),
                b'=' | b'+' => (b'=', b'+', "Equal", 187),
                b'[' | b'{' => (b'[', b'{', "BracketLeft", 219),
                b']' | b'}' => (b']', b'}', "BracketRight", 221),
                b'\\' | b'|' => (b'\\', b'|', "Backslash", 220),
                b';' | b':' => (b';', b':', "Semicolon", 186),
                b'\'' | b'"' => (b'\'', b'"', "Quote", 222),
                b',' | b'<' => (b',', b'<', "Comma", 188),
                b'.' | b'>' => (b'.', b'>', "Period", 190),
                b'/' | b'?' => (b'/', b'?', "Slash", 191),
                b'`' | b'~' => (b'`', b'~', "Backquote", 192),
                _ => bail!("Unsupported key: use printable ASCII or named keys"),
            };
            if c == shifted {
                modifiers |= 8;
            }
            (
                if modifiers & 8 != 0 { shifted } else { plain },
                code.to_owned(),
                vk,
            )
        };
        let key = (key as char).to_string();
        (key.clone(), code, vk, Some(key))
    };
    Ok(KeySpec {
        key,
        code,
        virtual_code,
        modifiers,
        // Shortcuts dispatch real keydown/up, never Input.insertText. Only
        // unmodified/Shift typing carries text for Chrome's default editing.
        text: if modifiers & 7 == 0 { text } else { None },
    })
}
const INSTALL_REVISION: &str = r#"(()=>{if(!globalThis.__celoom_dom_revision){let state={version:0};new MutationObserver(()=>state.version++).observe(document,{subtree:true,childList:true,attributes:true,characterData:true});Object.defineProperty(globalThis,'__celoom_dom_revision',{value:state,configurable:false,enumerable:false})}return true})()"#;
const SCREENSHOT_STAMP: &str = r#"({url:location.href,revision:globalThis.__celoom_dom_revision?.version??-1,scroll_x:scrollX,scroll_y:scrollY,width:innerWidth,height:innerHeight,dpr:devicePixelRatio})"#;
const COLLECT_NODES: &str = r#"(()=>{let out=[];function scan(root){for(const e of root.querySelectorAll('*')){if(out.length>=120)return;const s=getComputedStyle(e),r=e.getBoundingClientRect();if(s.display==='none'||s.visibility!=='visible'||r.width<=0||r.height<=0||e.tagName==='INPUT'&&e.type==='hidden')continue;if(e.matches('a,button,input,textarea,select,summary,[role],[contenteditable],[tabindex],label,h1,h2,h3,h4')||e.onclick)out.push(e);if(e.shadowRoot)scan(e.shadowRoot)}}scan(document);return out})()"#;
const METADATA: &str = r#"function(){const e=this,r=e.getBoundingClientRect(),s=getComputedStyle(e),t=(e.getAttribute('type')||'').toLowerCase(),tag=e.tagName.toLowerCase(),editable=e.isContentEditable||tag==='input'||tag==='textarea';const label=(e.getAttribute('aria-label')||Array.from(e.labels||[]).map(x=>x.innerText).join(' ')||e.getAttribute('title')||'').slice(0,160);return {tag,input_type:t,label,role:e.getAttribute('role')||'',name:label,identity_text:editable?'':(e.innerText||'').slice(0,160),text:(e.innerText||'').slice(0,160),value:t==='password'||t==='hidden'?null:('value'in e?String(e.value).slice(0,200):null),disabled:!!e.disabled||e.getAttribute('aria-disabled')==='true',readonly:!!e.readOnly,connected:e.isConnected,main_document:e.ownerDocument===document,visible:s.display!=='none'&&s.visibility==='visible'&&r.width>0&&r.height>0,rect:{x:r.x,y:r.y,width:r.width,height:r.height}}}"#;
const PREPARE_FILL: &str = r#"function(){const e=this,t=(e.type||'').toLowerCase();if(e.disabled||e.readOnly||!e.isConnected||!(e.isContentEditable||e.tagName==='TEXTAREA'||e.tagName==='INPUT'&&['text','search','email','url','tel','password','number',''].includes(t)))return false;e.focus();if(e.isContentEditable){let r=document.createRange();r.selectNodeContents(e);let s=getSelection();s.removeAllRanges();s.addRange(r)}else{e.select()}return true}"#;

#[cfg(test)]
mod tests {
    use super::*;
    fn test_network(mode: &str, proxy: &str, bypass: &str) -> NetworkConfig {
        NetworkConfig {
            proxy_mode: mode.into(),
            proxy_url: proxy.into(),
            no_proxy: bypass.into(),
            ..NetworkConfig::default()
        }
    }
    fn test_env(values: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let values: HashMap<String, String> = values
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |key| values.get(key).cloned()
    }
    #[test]
    fn browser_network_flags_are_explicit_and_credentials_are_rejected() {
        let direct = resolve_browser_route(
            &test_network("direct", "", ""),
            test_env(&[("HTTPS_PROXY", "http://ignored:80")]),
        )
        .unwrap();
        assert!(direct.flags().contains(&"--no-proxy-server".into()));
        assert!(
            !direct
                .flags()
                .iter()
                .any(|f| f.starts_with("--proxy-server="))
        );
        let manual = resolve_browser_route(
            &test_network("manual", "socks5h://[::1]:1081", ".example.com,127.0.0.1"),
            test_env(&[]),
        )
        .unwrap();
        assert!(
            manual
                .flags()
                .contains(&"--proxy-server=socks5://[::1]:1081".into())
        );
        assert_eq!(
            manual.bypass,
            "<-loopback>;example.com;*.example.com;127.0.0.1"
        );
        assert!(!manual.summary().to_string().contains("socks5h"));
        for proxy in [
            "http://private-user:private-password@proxy.test:80",
            "socks5://private-user@proxy.test:1080",
            "http://@proxy.test:80",
            "http://proxy.test/?token=private-password",
            "http://proxy.test;direct://",
        ] {
            let error = resolve_browser_route(&test_network("manual", proxy, ""), test_env(&[]))
                .unwrap_err()
                .to_string();
            assert!(
                !error.contains("private-password")
                    && !error.contains("private-user")
                    && !error.contains("proxy.test")
            );
        }
        assert!(
            resolve_browser_route(&test_network("manual", "ftp://proxy:21", ""), test_env(&[]))
                .is_err()
        );
    }
    #[test]
    fn browser_environment_proxy_matches_lowercase_protocol_and_all_fallback() {
        let env = test_env(&[
            ("http_proxy", "http://lower:8080"),
            ("HTTP_PROXY", "http://upper:8081"),
            ("https_proxy", "https://secure:444"),
            ("all_proxy", "socks5h://fallback:1080"),
            ("NO_PROXY", "localhost,.internal.test"),
        ]);
        let route = resolve_browser_route(&NetworkConfig::default(), env).unwrap();
        assert_eq!(
            route.proxy_server.as_deref(),
            Some("http=http://lower:8080;https=https://secure:444;socks=socks5://fallback:1080")
        );
        assert_eq!(
            route.bypass,
            "<-loopback>;localhost;*.localhost;internal.test;*.internal.test"
        );
        let cgi = resolve_browser_route(
            &NetworkConfig::default(),
            test_env(&[
                ("REQUEST_METHOD", "GET"),
                ("HTTP_PROXY", "http://untrusted:80"),
                ("ALL_PROXY", "socks5://all:1080"),
            ]),
        )
        .unwrap();
        assert_eq!(cgi.http.as_deref(), Some("socks5://all:1080"));
        let one = resolve_browser_route(
            &NetworkConfig::default(),
            test_env(&[("HTTPS_PROXY", "http://secure:80")]),
        )
        .unwrap();
        assert_eq!(
            one.proxy_server.as_deref(),
            Some("http=direct://;https=http://secure:80;socks=http://secure:80")
        );
        let empty = resolve_browser_route(&NetworkConfig::default(), test_env(&[])).unwrap();
        assert!(empty.flags().contains(&"--no-proxy-server".into()));
        let explicit = resolve_browser_route(
            &test_network("environment", "", "example.org"),
            test_env(&[("ALL_PROXY", "http://all:80"), ("no_proxy", "ignored.test")]),
        )
        .unwrap();
        assert_eq!(explicit.bypass, "<-loopback>;example.org;*.example.org");
    }
    #[test]
    fn mock_cdp_responses_do_not_confuse_events_or_echo_page_errors() {
        assert!(
            response_for(
                &json!({"method":"Page.frameNavigated","params":{}}),
                7,
                "Page.navigate"
            )
            .is_none()
        );
        assert!(response_for(&json!({"id":8,"result":{}}), 7, "Page.navigate").is_none());
        assert_eq!(
            response_for(
                &json!({"id":7,"result":{"loaderId":"loader"}}),
                7,
                "Page.navigate"
            )
            .unwrap()
            .unwrap()["loaderId"],
            "loader"
        );
        let error = response_for(
            &json!({"id":7,"error":{"code":-32000,"message":"private-page-content"}}),
            7,
            "DOM.resolveNode",
        )
        .unwrap()
        .unwrap_err()
        .to_string();
        assert!(!error.contains("private-page-content"));
        assert!(
            response_for(&json!({"id":7}), 7, "DOM.resolveNode")
                .unwrap()
                .is_err()
        );
    }
    #[test]
    fn endpoint_is_only_fresh_loopback_browser() {
        assert_eq!(
            endpoint_from_active_port("45231\n/devtools/browser/abc-123\n").unwrap(),
            "ws://127.0.0.1:45231/devtools/browser/abc-123"
        );
        for value in [
            "0\n/devtools/browser/a",
            "12\nws://evil/a",
            "9222\n/devtools/page/abc",
            "22\n/devtools/browser/a?x=y",
        ] {
            assert!(endpoint_from_active_port(value).is_err());
        }
    }
    #[test]
    fn url_key_and_geometry_validation() {
        for value in [
            "https://example.com/",
            "about:blank",
            "file:///tmp/fixture.html",
        ] {
            assert!(validate_url(value).is_ok());
        }
        for value in [
            "javascript:alert(1)",
            "data:text/html,hello",
            "https://u:p@example.com",
            "about:settings",
        ] {
            assert!(validate_url(value).is_err());
        }
        assert_eq!(parse_key("Ctrl+A").unwrap().modifiers, 2);
        assert_eq!(parse_key("Shift+Tab").unwrap().virtual_code, 9);
        assert!(parse_key("LaunchTerminal").is_err());
        assert!(check_point(1279.0, 799.0).is_ok());
        assert!(check_point(1280.0, 0.0).is_err());
    }
    #[test]
    fn shortcut_key_case_modifiers_and_named_aliases() {
        for (value, key, code, vk, modifiers, text) in [
            ("B", "b", "KeyB", 66, 0, Some("b")),
            ("b", "b", "KeyB", 66, 0, Some("b")),
            ("Shift+B", "B", "KeyB", 66, 8, Some("B")),
            ("Ctrl+B", "b", "KeyB", 66, 2, None),
            ("Control+Shift+b", "B", "KeyB", 66, 10, None),
            ("Alt+Meta+9", "9", "Digit9", 57, 5, None),
            ("1", "1", "Digit1", 49, 0, Some("1")),
            ("Shift+1", "!", "Digit1", 49, 8, Some("!")),
            ("!", "!", "Digit1", 49, 8, Some("!")),
            ("+", "+", "Equal", 187, 8, Some("+")),
            ("Ctrl++", "+", "Equal", 187, 10, None),
            ("Ctrl+Plus", "+", "Equal", 187, 10, None),
            ("Ctrl+Shift+=", "+", "Equal", 187, 10, None),
            ("Space", " ", "Space", 32, 0, Some(" ")),
            ("Spacebar", " ", "Space", 32, 0, Some(" ")),
            (" ", " ", "Space", 32, 0, Some(" ")),
            ("Return", "Enter", "Enter", 13, 0, Some("\r")),
            ("Ctrl+Enter", "Enter", "Enter", 13, 2, None),
            ("Shift+Tab", "Tab", "Tab", 9, 8, None),
            ("Esc", "Escape", "Escape", 27, 0, None),
            ("Del", "Delete", "Delete", 46, 0, None),
            ("Ins", "Insert", "Insert", 45, 0, None),
            ("PgUp", "PageUp", "PageUp", 33, 0, None),
            ("PgDn", "PageDown", "PageDown", 34, 0, None),
            ("Left", "ArrowLeft", "ArrowLeft", 37, 0, None),
            ("Shift+Slash", "?", "Slash", 191, 8, Some("?")),
        ] {
            let parsed = parse_key(value).unwrap();
            assert_eq!(
                (
                    parsed.key.as_str(),
                    parsed.code.as_str(),
                    parsed.virtual_code,
                    parsed.modifiers,
                    parsed.text.as_deref()
                ),
                (key, code, vk, modifiers, text),
                "{value}"
            );
        }
        for value in [
            "",
            "++",
            "Ctrl+",
            "+B",
            "Ctrl+++",
            "Hyper+B",
            "BB",
            "é",
            "\n",
            "\0",
            "LaunchTerminal",
        ] {
            assert!(parse_key(value).is_err(), "{value:?}");
        }
        assert!(parse_key(&"a".repeat(81)).is_err());
    }

    #[test]
    fn shortcut_keys_cover_ascii_letters_digits_and_punctuation() {
        for c in b'a'..=b'z' {
            for spelling in [c, c.to_ascii_uppercase()] {
                let parsed = parse_key(&(spelling as char).to_string()).unwrap();
                assert_eq!(parsed.key, (c as char).to_string());
                assert_eq!(
                    parsed.code,
                    format!("Key{}", c.to_ascii_uppercase() as char)
                );
                assert_eq!(parsed.virtual_code, c.to_ascii_uppercase() as u32);
                assert_eq!(parsed.modifiers, 0);
            }
        }
        for c in b'0'..=b'9' {
            let parsed = parse_key(&(c as char).to_string()).unwrap();
            assert_eq!(parsed.key, (c as char).to_string());
            assert_eq!(parsed.code, format!("Digit{}", c as char));
            assert_eq!(parsed.virtual_code, c as u32);
            assert_eq!(parsed.modifiers, 0);
        }
        for (plain, shifted, code, vk, alias) in [
            ("-", "_", "Minus", 189, "Minus"),
            ("=", "+", "Equal", 187, "Equals"),
            ("[", "{", "BracketLeft", 219, "LeftBracket"),
            ("]", "}", "BracketRight", 221, "RightBracket"),
            ("\\", "|", "Backslash", 220, "Backslash"),
            (";", ":", "Semicolon", 186, "Semicolon"),
            ("'", "\"", "Quote", 222, "Apostrophe"),
            (",", "<", "Comma", 188, "Comma"),
            (".", ">", "Period", 190, "Period"),
            ("/", "?", "Slash", 191, "Slash"),
            ("`", "~", "Backquote", 192, "Grave"),
        ] {
            for (value, expected, modifiers) in [
                (plain.to_owned(), plain, 0),
                (alias.to_owned(), plain, 0),
                (shifted.to_owned(), shifted, 8),
                (format!("Shift+{plain}"), shifted, 8),
                (format!("Ctrl+{plain}"), plain, 2),
            ] {
                let parsed = parse_key(&value).unwrap();
                assert_eq!(parsed.key, expected, "{value}");
                assert_eq!(parsed.code, code, "{value}");
                assert_eq!(parsed.virtual_code, vk, "{value}");
                assert_eq!(parsed.modifiers, modifiers, "{value}");
                assert_eq!(
                    parsed.text.as_deref(),
                    if modifiers & 7 == 0 {
                        Some(expected)
                    } else {
                        None
                    },
                    "{value}"
                );
            }
        }
        for c in b'!'..=b'~' {
            assert!(parse_key(&(c as char).to_string()).is_ok(), "{c}");
        }
    }

    /// Local fixture only; the public controller API supplies all input/observations.
    #[tokio::test]
    #[ignore = "launches a fresh private headless Chrome; run explicitly"]
    async fn private_browser_global_shortcuts_smoke() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = dir.path().join("global-shortcuts.html");
        tokio::fs::write(
            &fixture,
            r#"<!doctype html><meta charset=utf-8>
<title>Private global keyboard fixture</title><body tabindex=0>
<h1>Global shortcuts, noneditable body</h1><script>
document.body.focus();
for (const type of ['keydown', 'keyup']) {
  window.addEventListener(type, e => {
    const record = document.createElement('h3');
    record.textContent = JSON.stringify([e.type, e.key, e.code,
      e.shiftKey, e.ctrlKey, e.altKey, e.metaKey, e.isTrusted,
      e.target.tagName, e.target.isContentEditable]);
    document.body.append(record);
    e.preventDefault();
  });
}
</script>"#,
        )
        .await
        .unwrap();
        let controller = BrowserController::new(dir.path().into());
        let action = |name: &str| -> ComputerAction {
            serde_json::from_value(json!({"action":name,"target":"browser","ref_id":null,"url":null,"text":null,"x":null,"y":null,"key":null,"button":null,"delta":null,"argv":null})).unwrap()
        };
        let test: Result<()> = async {
            controller
                .configure_network(test_network("direct", "", ""))
                .await?;
            let mut open = action("open");
            open.url = Some(url::Url::from_file_path(&fixture).unwrap().to_string());
            controller
                .execute("global-keys", &open, CancellationToken::new())
                .await?;
            let mut typing = action("type");
            typing.text = Some("b".into());
            assert!(
                controller
                    .execute("global-keys", &typing, CancellationToken::new())
                    .await
                    .err()
                    .unwrap()
                    .to_string()
                    .starts_with("NO_EDITABLE_FOCUS:")
            );

            let cases = [
                ("B", "b", "KeyB", false, false),
                ("b", "b", "KeyB", false, false),
                ("0", "0", "Digit0", false, false),
                ("9", "9", "Digit9", false, false),
                ("Shift+B", "B", "KeyB", true, false),
                ("Ctrl+B", "b", "KeyB", false, true),
                ("Ctrl+Shift+B", "B", "KeyB", true, true),
                ("Shift+1", "!", "Digit1", true, false),
                ("/", "/", "Slash", false, false),
                ("Shift+/", "?", "Slash", true, false),
                ("+", "+", "Equal", true, false),
                ("Ctrl++", "+", "Equal", true, true),
                ("Space", " ", "Space", false, false),
                ("Enter", "Enter", "Enter", false, false),
            ];
            let mut expected = Vec::new();
            for (value, key, code, shift, ctrl) in cases {
                let mut press = action("key");
                press.key = Some(value.into());
                let result = controller
                    .execute("global-keys", &press, CancellationToken::new())
                    .await?;
                assert_eq!(
                    serde_json::from_str::<Value>(&result.text)?["outcome"],
                    "applied"
                );
                for event in ["keydown", "keyup"] {
                    expected.push(json!([
                        event, key, code, shift, ctrl, false, false, true, "BODY", false
                    ]));
                }
            }
            let observed = controller
                .execute("global-keys", &action("observe"), CancellationToken::new())
                .await?;
            let snapshot: Value = serde_json::from_str(&observed.text)?;
            let actual: Vec<Value> = snapshot["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|node| node["tag"] == "h3")
                .map(|node| serde_json::from_str(node["text"].as_str().unwrap()).unwrap())
                .collect();
            assert_eq!(actual, expected);
            // key succeeds on BODY but never relaxes type's editable-focus guard.
            assert!(
                controller
                    .execute("global-keys", &typing, CancellationToken::new())
                    .await
                    .err()
                    .unwrap()
                    .to_string()
                    .starts_with("NO_EDITABLE_FOCUS:")
            );
            Ok(())
        }
        .await;
        controller.stop(Some("global-keys")).await.unwrap();
        test.unwrap();
        assert_eq!(controller.status().await["running"], false);
    }

    #[tokio::test]
    async fn status_and_observe_never_start_browser() {
        let dir = tempfile::tempdir().unwrap();
        let controller = BrowserController::new(dir.path().into());
        assert_eq!(controller.status().await["running"], false);
        let a:ComputerAction=serde_json::from_value(json!({"action":"observe","target":"browser","ref_id":null,"url":null,"text":null,"x":null,"y":null,"key":null,"button":null,"delta":null,"argv":null})).unwrap();
        assert!(
            controller
                .execute("test", &a, CancellationToken::new())
                .await
                .is_err()
        );
        assert!(!dir.path().join("computer").exists());
    }
    /// Explicit offline, self-owned Chrome test. Never connects to an existing browser.
    #[tokio::test]
    #[ignore = "launches a fresh private headless Chrome; run explicitly"]
    async fn private_browser_offline_smoke() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = dir.path().join("fixture.html");
        tokio::fs::write(&fixture,"<!doctype html><meta charset=utf-8><title>AI offline fixture</title><label>Name<input id=name></label><input type=password value=secret><button id=save onclick=\"document.querySelector('#result').textContent=document.querySelector('#name').value;this.textContent='Saved'\">Save</button><p id=result></p>").await.unwrap();
        let controller = BrowserController::new(dir.path().into());
        let action = |name: &str| -> ComputerAction {
            serde_json::from_value(json!({"action":name,"target":"browser","ref_id":null,"url":null,"text":null,"x":null,"y":null,"key":null,"button":null,"delta":null,"argv":null})).unwrap()
        };
        let test: Result<()> = async {
            let direct = test_network("direct", "", "");
            controller.configure_network(direct.clone()).await?;
            let mut a = action("open");
            a.url = Some(url::Url::from_file_path(&fixture).unwrap().to_string());
            let observation = controller
                .execute("test", &a, CancellationToken::new())
                .await?;
            let snapshot: Value = serde_json::from_str(&observation.text)?;
            controller.configure_network(direct.clone()).await?;
            let changed = controller
                .configure_network(test_network("manual", "http://127.0.0.1:9", ""))
                .await;
            assert!(
                changed
                    .unwrap_err()
                    .to_string()
                    .starts_with("BROWSER_NETWORK_CHANGED:")
            );
            assert_eq!(controller.status().await["network"]["proxy_enabled"], false);
            assert!(!observation.text.contains("secret"));
            let nodes = snapshot["nodes"].as_array().unwrap();
            let name_ref = nodes
                .iter()
                .find(|n| n["tag"] == "input" && n["input_type"] != "password")
                .unwrap()["ref_id"]
                .as_str()
                .unwrap()
                .to_owned();
            let button_ref = nodes.iter().find(|n| n["tag"] == "button").unwrap()["ref_id"]
                .as_str()
                .unwrap()
                .to_owned();
            let mut fill = action("fill");
            fill.ref_id = Some(name_ref.clone());
            fill.text = Some("offline hello 世界".into());
            controller
                .execute("test", &fill, CancellationToken::new())
                .await?;
            let mut key = action("key");
            key.key = Some("Ctrl+A".into());
            controller
                .execute("test", &key, CancellationToken::new())
                .await?;
            let mut typing = action("type");
            typing.text = Some("offline hello 世界".into());
            controller
                .execute("test", &typing, CancellationToken::new())
                .await?;
            let mut click = action("click");
            click.ref_id = Some(button_ref);
            controller
                .execute("test", &click, CancellationToken::new())
                .await?;
            let stale = controller
                .execute("test", &click, CancellationToken::new())
                .await;
            assert!(stale.err().unwrap().to_string().contains("STALE_REF"));
            let observed = controller
                .execute("test", &action("observe"), CancellationToken::new())
                .await?;
            assert!(observed.text.contains("offline hello 世界"));
            assert!(
                controller
                    .execute("test", &fill, CancellationToken::new())
                    .await
                    .is_err()
            );
            assert!(
                controller
                    .execute("other-owner", &action("observe"), CancellationToken::new())
                    .await
                    .is_err()
            );
            let screenshot = controller
                .execute("test", &action("screenshot"), CancellationToken::new())
                .await?;
            let image = screenshot.image.unwrap();
            assert_eq!((image.width, image.height), (WIDTH, HEIGHT));
            assert!(image.png.starts_with(b"\x89PNG"));
            let mut point = action("click");
            point.x = Some(5);
            point.y = Some(5);
            controller
                .execute("test", &point, CancellationToken::new())
                .await?;
            assert!(
                controller
                    .execute("test", &point, CancellationToken::new())
                    .await
                    .is_err()
            );
            let mut scroll = action("scroll");
            scroll.delta = Some(100);
            controller
                .execute("test", &scroll, CancellationToken::new())
                .await?;
            let mut blank = action("open");
            blank.url = Some("about:blank".into());
            controller
                .execute("test", &blank, CancellationToken::new())
                .await?;
            assert!(
                controller
                    .execute("test", &fill, CancellationToken::new())
                    .await
                    .is_err()
            );
            Ok(())
        }
        .await;
        controller.stop(Some("test")).await.unwrap();
        test.unwrap();
        assert_eq!(controller.status().await["running"], false);
    }
}
