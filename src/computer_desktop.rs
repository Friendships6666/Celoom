//! Isolated KDE Wayland desktop. There is deliberately no implicit host fallback.
use crate::computer::{ComputerAction, ComputerImage, ComputerOutput};
use crate::computer_fake_input::org_kde_kwin_fake_input::OrgKdeKwinFakeInput;
use anyhow::{Context, Result, bail, ensure};
use image::ImageEncoder;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom},
    os::{
        fd::AsFd,
        unix::{
            fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
            net::UnixStream,
        },
    },
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex as StdMutex},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncSeekExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::Mutex,
};
use tokio_util::sync::CancellationToken;
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle,
    protocol::{wl_callback, wl_output, wl_registry},
};

const HELPER: &str = include_str!("../helpers/native_accessibility.py");
const VISION_HELPER: &str = include_str!("../helpers/native_vision.py");
const HANDOFF_HELPER: &str = include_str!("../helpers/desktop_handoff.py");
const WIDTH: u32 = 1280;
const HEIGHT: u32 = 800;
const MAX_IMAGE: usize = 256 * 1024 * 1024;

#[derive(Clone)]
pub struct DesktopController {
    inner: Arc<Inner>,
}
struct Inner {
    _data_dir: PathBuf,
    session: Mutex<Option<Session>>,
    cancel: StdMutex<Option<CancellationToken>>,
    host: Mutex<Option<HostSession>>,
    host_env: Option<HostEnvironment>,
    host_cancel: StdMutex<Option<CancellationToken>>,
}
#[derive(Clone)]
struct HostEnvironment {
    socket: PathBuf,
    bus: String,
}
struct HostSession {
    owner: String,
    bus: zbus::Connection,
    cancel: CancellationToken,
    last_capture: Option<Instant>,
    geometry: Option<(u32, u32)>,
    expires: Instant,
}
struct StartupRoot {
    path: PathBuf,
    keep: bool,
}
impl Drop for StartupRoot {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}
fn original_host_environment() -> Option<HostEnvironment> {
    let runtime = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR")?);
    let display = PathBuf::from(std::env::var_os("WAYLAND_DISPLAY")?);
    let socket = if display.is_absolute() {
        display
    } else {
        runtime.join(display)
    };
    let bus = std::env::var("DBUS_SESSION_BUS_ADDRESS").ok()?;
    if !socket.is_absolute() || !bus.starts_with("unix:") {
        return None;
    }
    Some(HostEnvironment { socket, bus })
}
struct Bridge {
    child: ManagedChild,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}
struct ManagedChild {
    child: Child,
    pgid: i32,
}
impl ManagedChild {
    fn new(child: Child) -> Self {
        let pgid = child.id().expect("newly spawned child has pid") as i32;
        Self { child, pgid }
    }
}
impl std::ops::Deref for ManagedChild {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.child
    }
}
impl std::ops::DerefMut for ManagedChild {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.child
    }
}
impl Drop for ManagedChild {
    fn drop(&mut self) {
        terminate(self);
    }
}
struct DesktopHandoff {
    stop: CancellationToken,
    viewer: ManagedChild,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for DesktopHandoff {
    fn drop(&mut self) {
        self.stop.cancel();
        self.task.abort();
        terminate(&mut self.viewer);
    }
}
struct Session {
    owner: String,
    root: PathBuf,
    display: String,
    env: HashMap<String, String>,
    host_env: Option<HostEnvironment>,
    compositor: ManagedChild,
    apps: Vec<ManagedChild>,
    registry: Option<ManagedChild>,
    bridge: Option<Bridge>,
    handoff: Option<DesktopHandoff>,
    bus: zbus::Connection,
    cancel: CancellationToken,
    last_capture: Option<Instant>,
}

fn terminate(child: &mut ManagedChild) {
    // Every managed process is spawned into its own fresh process group.
    let _ = nix::sys::signal::killpg(
        nix::unistd::Pid::from_raw(child.pgid),
        nix::sys::signal::Signal::SIGKILL,
    );
    let _ = child.start_kill();
}
impl Drop for Session {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.handoff.take();
        if let Some(bridge) = &mut self.bridge {
            terminate(&mut bridge.child);
        }
        for app in &mut self.apps {
            terminate(app);
        }
        if let Some(registry) = &mut self.registry {
            terminate(registry);
        }
        terminate(&mut self.compositor);
        // root was generated and created exclusively by this controller, never user-supplied.
        if self.root.parent() == Some(Path::new("/tmp"))
            && self
                .root
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("celoom-desktop-"))
        {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

fn registry_binary() -> Option<PathBuf> {
    [
        "/usr/lib/at-spi2-registryd",
        "/usr/libexec/at-spi2-registryd",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|p| p.is_file())
}
fn binary(name: &str) -> Option<PathBuf> {
    ["/usr/bin", "/bin", "/usr/local/bin"]
        .into_iter()
        .map(|p| Path::new(p).join(name))
        .find(|p| {
            p.metadata()
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}
fn command(path: impl AsRef<std::ffi::OsStr>, env: &HashMap<String, String>) -> Command {
    // Fixed util-linux launcher applies inherited resource limits without
    // pre_exec/unsafe code. Do not cap virtual address space: GPU/Mesa drivers
    // reserve large sparse mappings, unrelated to actual memory consumption.
    let mut cmd = Command::new("/usr/bin/prlimit");
    cmd.arg("--core=0").arg("--nofile=4096").arg("--").arg(path);
    cmd.env_clear()
        .envs(env)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    cmd.process_group(0);
    cmd
}
fn text_output(text: impl Into<String>) -> ComputerOutput {
    ComputerOutput {
        text: text.into(),
        image: None,
    }
}

impl DesktopController {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            inner: Arc::new(Inner {
                _data_dir: data_dir,
                session: Mutex::new(None),
                cancel: StdMutex::new(None),
                host: Mutex::new(None),
                host_env: original_host_environment(),
                host_cancel: StdMutex::new(None),
            }),
        }
    }
    pub async fn status(&self) -> Value {
        let dependencies = json!({"kwin_wayland":binary("kwin_wayland"),"dbus_run_session":binary("dbus-run-session"),"python3":binary("python3"),"prlimit":binary("prlimit"),"atspi_registry":registry_binary()});
        let guard = self.inner.session.try_lock();
        let (running, owner) = match &guard {
            Ok(s) => (s.is_some(), s.as_ref().map(|s| s.owner.clone())),
            Err(_) => (true, None),
        };
        json!({"supported_platform":"Linux KDE Wayland", "available":binary("kwin_wayland").is_some() && binary("dbus-run-session").is_some() && binary("python3").is_some() && Path::new("/usr/bin/prlimit").is_file(),
            "running":running,"owner":owner,"busy":guard.is_err(),"dependencies":dependencies,
            "ocr_available":binary("tesseract").is_some(),"handoff_available":self.inner.host_env.is_some(),
            "isolation":"private virtual KWin + private Wayland, D-Bus, isolated empty HOME/config/cache (does NOT contain host app logins; use action='handoff' on desktop for user login/2FA, then 'resume_headless')",
            "viewport":{"width":WIDTH,"height":HEIGHT},"native_accessibility":"fixed Python GI AT-SPI helper",
            "capabilities":["launch","observe","screenshot","ocr","click","double_click","right_click","click_text","find_color","click_color","fill","type","key","scroll","wait","handoff","resume_headless","stop"],
            "limits":{"apps":8,"operation_seconds":30,"accessibility_nodes":512,"raw_capture_bytes":MAX_IMAGE},
            "host":{"available":self.inner.host_env.is_some(),"requires":"explicit main host lease + live physical input monitor + human idle gate", "accessibility":false,"launch":false,"ocr":true,"color_click":true,"reason":"Host uses constructor-captured socket/bus only; host supports screenshot, ocr, click_text, find_color, click_color, wait, click, double_click, right_click, type, key, scroll"},
            "limitations":["Not a filesystem/network security sandbox", "Apps must support Wayland; X11-only apps unsupported", "Private desktop starts with empty HOME; use handoff for user logins"]})
    }
    pub async fn stop(&self, owner: Option<&str>) -> Result<()> {
        // Signal before waiting for the operation mutex, so an in-flight operation releases it.
        if owner.is_none()
            && let Some(cancel) = self
                .inner
                .cancel
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
        {
            cancel.cancel();
        }
        if owner.is_none()
            && let Some(cancel) = self
                .inner
                .host_cancel
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
        {
            cancel.cancel();
        }
        let mut host = self.inner.host.lock().await;
        if let Some(session) = host.as_ref() {
            ensure!(
                owner.is_none_or(|o| o == session.owner),
                "Host desktop is busy: lease belongs to another session"
            );
        }
        if let Some(session) = host.take() {
            session.cancel.cancel();
        }
        *self
            .inner
            .host_cancel
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
        let mut guard = self.inner.session.lock().await;
        if let Some(session) = guard.as_ref() {
            ensure!(
                owner.is_none_or(|o| o == session.owner),
                "Desktop is busy: lease belongs to another session"
            );
        }
        if let Some(mut session) = guard.take() {
            session.cancel.cancel();
            session.shutdown().await;
        }
        *self.inner.cancel.lock().unwrap_or_else(|e| e.into_inner()) = None;
        Ok(())
    }
    /// Main's host guard must grant the explicit short lease and supply its physical-
    /// activity cancellation token. No host process/helper is launched by this path.
    pub async fn execute_host(
        &self,
        owner: &str,
        action: &ComputerAction,
        cancel: CancellationToken,
    ) -> Result<ComputerOutput> {
        if action.action == "status" {
            return Ok(text_output(self.status().await.to_string()));
        }
        if action.action == "stop" {
            self.stop(Some(owner)).await?;
            return Ok(text_output(
                "Desktop leases released; host compositor was not terminated",
            ));
        }
        ensure!(
            !cancel.is_cancelled(),
            "Host lease cancelled before dispatch"
        );
        let env = self.inner.host_env.clone().context(
            "Host Wayland socket/session bus was not present when controller was created",
        )?;
        let mut guard = tokio::select! {_=cancel.cancelled()=>bail!("Host lease cancelled"),guard=self.inner.host.lock()=>guard};
        if guard.as_ref().is_some_and(|s| s.expires < Instant::now())
            && let Some(old) = guard.take()
        {
            old.cancel.cancel();
        }
        if let Some(session) = guard.as_ref() {
            ensure!(
                session.owner == owner,
                "Host desktop is busy: lease belongs to another session"
            );
        }
        if guard.is_none() {
            let bus = tokio::select! {_=cancel.cancelled()=>bail!("Host lease cancelled"),result=zbus::connection::Builder::address(env.bus.as_str())?.build()=>result?};
            let shutdown = CancellationToken::new();
            *self
                .inner
                .host_cancel
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(shutdown.clone());
            *guard = Some(HostSession {
                owner: owner.into(),
                bus,
                cancel: shutdown,
                last_capture: None,
                geometry: None,
                expires: Instant::now() + Duration::from_secs(30),
            });
        }
        let session = guard.as_mut().unwrap();
        session.expires = Instant::now() + Duration::from_secs(30);
        let shutdown = session.cancel.clone();
        let operation = async {
            match action.action.as_str() {
                "observe" | "screenshot" => {
                    let image = capture_workspace(&session.bus).await?;
                    session.geometry = Some((image.width, image.height));
                    session.last_capture = Some(Instant::now());
                    Ok(ComputerOutput {
                        text: format!(
                            "EXPLICIT HOST screenshot {}x{}. Physical pixels; requires scale=1 for coordinates. Host AT-SPI is intentionally unavailable; this is not the isolated desktop.",
                            image.width, image.height
                        ),
                        image: Some(image),
                    })
                }
                "wait" => {
                    let ms = action.delta.unwrap_or(500).clamp(1, 30_000) as u64;
                    tokio::time::sleep(Duration::from_millis(ms)).await;
                    Ok(text_output(
                        json!({"ok":true,"action":"wait","target":"host","waited_ms":ms}).to_string(),
                    ))
                }
                "ocr" => {
                    let image = capture_workspace(&session.bus).await?;
                    session.geometry = Some((image.width, image.height));
                    session.last_capture = Some(Instant::now());
                    let ocr = run_vision(&image.png, &["ocr"]).await?;
                    Ok(text_output(ocr.to_string()))
                }
                "find_color" => {
                    let color = action.color.as_deref().context("find_color requires color (#RRGGBB or R,G,B)")?;
                    let tol = action.delta.unwrap_or(24).clamp(0, 255).to_string();
                    let image = capture_workspace(&session.bus).await?;
                    session.geometry = Some((image.width, image.height));
                    session.last_capture = Some(Instant::now());
                    let found = run_vision(&image.png, &["find_color", color, &tol]).await?;
                    Ok(text_output(found.to_string()))
                }
                "click_text" => {
                    let query = action.text.as_deref().filter(|s| !s.trim().is_empty()).context("click_text requires text")?;
                    let image = capture_workspace(&session.bus).await?;
                    let geometry = (image.width, image.height);
                    session.geometry = Some(geometry);
                    let ocr = run_vision(&image.png, &["ocr"]).await?;
                    let (matched, cx, cy, bbox) = pick_ocr_match(&ocr, query)?;
                    let click_action = ComputerAction {
                        action: "click".into(), target: action.target.clone(), ref_id: None, url: None,
                        text: None, x: Some(cx), y: Some(cy), key: None, button: action.button.clone(),
                        delta: None, argv: None, max_nodes: None, max_depth: None, color: None,
                    };
                    let commands = input_commands_with_geometry(&click_action, geometry.0, geometry.1)?;
                    let socket = env.socket.clone();
                    let token = cancel.clone();
                    let emergency = shutdown.clone();
                    tokio::task::spawn_blocking(move || {
                        send_input_scoped(&socket, commands, true, token, emergency, Some(geometry))
                    })
                    .await??;
                    session.last_capture = None;
                    Ok(text_output(json!({"ok":true,"action":"click_text","target":"host","matched_text":matched,"cx":cx,"cy":cy,"bbox":bbox}).to_string()))
                }
                "click_color" => {
                    let color = action.color.as_deref().context("click_color requires color (#RRGGBB or R,G,B)")?;
                    let tol = action.delta.unwrap_or(24).clamp(0, 255).to_string();
                    let image = capture_workspace(&session.bus).await?;
                    let geometry = (image.width, image.height);
                    session.geometry = Some(geometry);
                    let found = run_vision(&image.png, &["find_color", color, &tol]).await?;
                    let cluster = found["clusters"].as_array().and_then(|a| a.first()).context("No matching color cluster found on screen")?;
                    let cx = cluster["cx"].as_i64().context("Missing cluster cx")? as i32;
                    let cy = cluster["cy"].as_i64().context("Missing cluster cy")? as i32;
                    let click_action = ComputerAction {
                        action: "click".into(), target: action.target.clone(), ref_id: None, url: None,
                        text: None, x: Some(cx), y: Some(cy), key: None, button: action.button.clone(),
                        delta: None, argv: None, max_nodes: None, max_depth: None, color: None,
                    };
                    let commands = input_commands_with_geometry(&click_action, geometry.0, geometry.1)?;
                    let socket = env.socket.clone();
                    let token = cancel.clone();
                    let emergency = shutdown.clone();
                    tokio::task::spawn_blocking(move || {
                        send_input_scoped(&socket, commands, true, token, emergency, Some(geometry))
                    })
                    .await??;
                    session.last_capture = None;
                    Ok(text_output(json!({"ok":true,"action":"click_color","target":"host","color":color,"cx":cx,"cy":cy,"cluster":cluster}).to_string()))
                }
                "click" | "double_click" | "right_click" | "scroll" | "key" | "type" => {
                    ensure!(
                        action.ref_id.is_none(),
                        "Host accessibility refs are unsupported; use explicit screenshot coordinates or ocr/click_text/click_color"
                    );
                    ensure!(
                        session
                            .last_capture
                            .is_some_and(|t| t.elapsed() < Duration::from_secs(10)),
                        "Take a fresh explicit host screenshot before each input action"
                    );
                    // Consume the observation before submitting a side effect;
                    // failure never licenses a blind repeat into changed focus.
                    session.last_capture = None;
                    let geometry = session.geometry.unwrap_or((WIDTH, HEIGHT));
                    let commands = input_commands_with_geometry(action, geometry.0, geometry.1)?;
                    let socket = env.socket.clone();
                    let token = cancel.clone();
                    let emergency = shutdown.clone();
                    tokio::task::spawn_blocking(move || {
                        send_input_scoped(&socket, commands, true, token, emergency, Some(geometry))
                    })
                    .await??;
                    Ok(text_output(
                        json!({"ok":true,"action":action.action,"target":"host","x":action.x,"y":action.y,"key":action.key,"delta":action.delta,"note":"Input submitted to host desktop"}).to_string(),
                    ))
                }
                "fill" => bail!(
                    "Host AT-SPI fill is unavailable; fixed helper is restricted to private apps"
                ),
                "launch" | "open" => bail!(
                    "Host application launch is unsupported; launch only on the private desktop"
                ),
                _ => bail!("Unsupported explicit host operation"),
            }
        };
        let result = tokio::select! {_=cancel.cancelled()=>Err(anyhow::anyhow!("Host lease cancelled by physical activity/expiry; input releases queued")),_=shutdown.cancelled()=>Err(anyhow::anyhow!("Host desktop emergency stop")),result=tokio::time::timeout(Duration::from_secs(25),operation)=>match result{Ok(result)=>result,Err(_)=>{shutdown.cancel();Err(anyhow::anyhow!("Host action timed out"))}}};
        if cancel.is_cancelled() || shutdown.is_cancelled() {
            guard.take();
        }
        result
    }
    pub async fn execute(
        &self,
        owner: &str,
        action: &ComputerAction,
        cancel: CancellationToken,
    ) -> Result<ComputerOutput> {
        if action.action == "status" {
            return Ok(text_output(self.status().await.to_string()));
        }
        if action.action == "stop" {
            self.stop(Some(owner)).await?;
            return Ok(text_output("Private desktop stopped"));
        }
        ensure!(!cancel.is_cancelled(), "Desktop action cancelled");
        let mut guard = tokio::select! { _=cancel.cancelled()=>bail!("Desktop action cancelled"), guard=self.inner.session.lock()=>guard };
        if let Some(session) = guard.as_ref() {
            ensure!(
                session.owner == owner,
                "Desktop is busy: lease belongs to another session"
            );
        }
        if guard.is_none() {
            let session_cancel = CancellationToken::new();
            *self.inner.cancel.lock().unwrap_or_else(|e| e.into_inner()) =
                Some(session_cancel.clone());
            let started = tokio::select! {
                _=cancel.cancelled()=>bail!("Desktop startup cancelled"),
                _=session_cancel.cancelled()=>bail!("Desktop startup stopped"),
                started=tokio::time::timeout(Duration::from_secs(20),Session::start(owner,session_cancel.clone(),self.inner.host_env.clone()))=>started.context("Private desktop startup timed out")??,
            };
            *guard = Some(started);
        }
        let session = guard.as_mut().unwrap();
        let shutdown = session.cancel.clone();
        let result = tokio::select! {
            _=cancel.cancelled()=>Err(anyhow::anyhow!("Desktop action cancelled; observe before retrying side effects")),
            _=shutdown.cancelled()=>Err(anyhow::anyhow!("Private desktop stopped")),
            result=tokio::time::timeout(Duration::from_secs(30),session.execute(action))=>match result{Ok(result)=>result,Err(_)=>Err(anyhow::anyhow!("Desktop action timed out; side effect outcome may be unknown"))},
        };
        if (cancel.is_cancelled()
            || shutdown.is_cancelled()
            || result
                .as_ref()
                .is_err_and(|e| e.to_string().contains("timed out")))
            && let Some(mut session) = guard.take()
        {
            session.shutdown().await;
        }
        result
    }
}

impl Session {
    async fn start(
        owner: &str,
        cancel: CancellationToken,
        host_env: Option<HostEnvironment>,
    ) -> Result<Self> {
        let kwin =
            binary("kwin_wayland").context("Install KWin Wayland for the private desktop")?;
        let dbus = binary("dbus-run-session")
            .context("Install dbus-run-session for the private desktop")?;
        let python =
            binary("python3").context("Install Python 3 and GI Atspi for native accessibility")?;
        let id = uuid::Uuid::new_v4().simple().to_string();
        let root = PathBuf::from(format!("/tmp/celoom-desktop-{id}"));
        std::fs::DirBuilder::new().mode(0o700).create(&root)?;
        let mut cleanup = StartupRoot {
            path: root.clone(),
            keep: false,
        };
        let result = Self::start_at(owner, cancel, host_env, &root, &id, kwin, dbus, python).await;
        if result.is_ok() {
            cleanup.keep = true;
        }
        result
    }
    async fn start_at(
        owner: &str,
        cancel: CancellationToken,
        host_env: Option<HostEnvironment>,
        root: &Path,
        id: &str,
        kwin: PathBuf,
        dbus: PathBuf,
        python: PathBuf,
    ) -> Result<Self> {
        let display = format!("celoom-{id}");
        for sub in ["home", "config", "cache", "data"] {
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(root.join(sub))?;
        }
        let helper = root.join("native_accessibility.py");
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&helper)?;
        std::io::Write::write_all(&mut file, HELPER.as_bytes())?;
        let mut env = HashMap::from([
            ("PATH".into(), "/usr/local/bin:/usr/bin:/bin".into()),
            ("LANG".into(), "C.UTF-8".into()),
            (
                "HOME".into(),
                root.join("home").to_string_lossy().into_owned(),
            ),
            (
                "XDG_RUNTIME_DIR".into(),
                root.to_string_lossy().into_owned(),
            ),
            (
                "XDG_CONFIG_HOME".into(),
                root.join("config").to_string_lossy().into_owned(),
            ),
            (
                "XDG_CACHE_HOME".into(),
                root.join("cache").to_string_lossy().into_owned(),
            ),
            (
                "XDG_DATA_HOME".into(),
                root.join("data").to_string_lossy().into_owned(),
            ),
            ("XDG_SESSION_TYPE".into(), "wayland".into()),
            ("QT_QPA_PLATFORM".into(), "wayland".into()),
            ("GDK_BACKEND".into(), "wayland".into()),
            ("QT_LINUX_ACCESSIBILITY_ALWAYS_ON".into(), "1".into()),
            ("GTK_A11Y".into(), "atspi".into()),
            ("LIBGL_ALWAYS_SOFTWARE".into(), "1".into()),
        ]);
        let ready = root.join("ready.json");
        let log = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(root.join("kwin.log"))?;
        let mut compositor = ManagedChild::new(
            command(dbus, &env)
                .arg("--")
                .arg(&python)
                .arg("-I")
                .arg("-c")
                .arg(HELPER)
                .arg("bootstrap")
                .arg(&ready)
                .arg(&display)
                .arg(kwin)
                .stderr(log)
                .spawn()
                .context("Start isolated dbus-run-session KWin")?,
        );
        // Until Session exists, this guard kills and reaps startup cancellation too.
        let startup = async {
            let start = Instant::now();
            let value = loop {
                ensure!(
                    compositor.try_wait()?.is_none(),
                    "Private KWin exited during startup (virtual backend may be unavailable)"
                );
                if let Ok(bytes) = tokio::fs::read(&ready).await
                    && let Ok(value) = serde_json::from_slice::<Value>(&bytes)
                    && root.join(&display).exists()
                {
                    break value;
                }
                ensure!(
                    start.elapsed() < Duration::from_secs(15),
                    "Private KWin did not create its Wayland socket"
                );
                tokio::time::sleep(Duration::from_millis(50)).await;
            };
            Ok::<Value, anyhow::Error>(value)
        }
        .await;
        let ready = match startup {
            Ok(v) => v,
            Err(e) => {
                terminate(&mut compositor);
                let _ = compositor.wait().await;
                return Err(e);
            }
        };
        let address = ready["bus_address"]
            .as_str()
            .context("Private bootstrap did not report D-Bus address")?;
        ensure!(
            address.starts_with("unix:"),
            "Invalid private D-Bus address"
        );
        env.insert("DBUS_SESSION_BUS_ADDRESS".into(), address.into());
        env.insert("WAYLAND_DISPLAY".into(), display.clone());
        let bus = match zbus::connection::Builder::address(address)?.build().await {
            Ok(bus) => bus,
            Err(e) => {
                terminate(&mut compositor);
                let _ = compositor.wait().await;
                return Err(e.into());
            }
        };
        let dbus_proxy = zbus::fdo::DBusProxy::new(&bus).await?;
        let started = Instant::now();
        while !dbus_proxy
            .name_has_owner(zbus::names::BusName::try_from("org.kde.KWin")?)
            .await?
        {
            ensure!(
                compositor.try_wait()?.is_none(),
                "Private compositor exited before D-Bus ready"
            );
            ensure!(
                started.elapsed() < Duration::from_secs(10),
                "Private KWin D-Bus service was not registered"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        // Avoid distro AT-SPI activation via the host user-systemd. AT-SPI can
        // use this already private session bus directly, with a managed registry.
        env.insert("AT_SPI_BUS_ADDRESS".into(), address.into());
        let registry = if let Some(path) = registry_binary() {
            Some(ManagedChild::new(
                command(path, &env)
                    .spawn()
                    .context("Start private accessibility registry")?,
            ))
        } else {
            None
        };
        if registry.is_some() {
            let dbus = zbus::fdo::DBusProxy::new(&bus).await?;
            for _ in 0..40 {
                if dbus
                    .name_has_owner(zbus::names::BusName::try_from("org.a11y.atspi.Registry")?)
                    .await?
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        }
        Ok(Self {
            owner: owner.into(),
            root: root.into(),
            display,
            env,
            compositor,
            apps: vec![],
            registry,
            bridge: None,
            bus,
            cancel,
            last_capture: None,
            host_env,
            handoff: None,
        })
    }
    async fn shutdown(&mut self) {
        self.cancel.cancel();
        if let Some(mut bridge) = self.bridge.take() {
            terminate(&mut bridge.child);
            let _ = bridge.child.wait().await;
        }
        for app in &mut self.apps {
            terminate(app);
            let _ = app.wait().await;
        }
        if let Some(mut registry) = self.registry.take() {
            terminate(&mut registry);
            let _ = registry.wait().await;
        }
        terminate(&mut self.compositor);
        let _ = self.compositor.wait().await;
    }
    fn pids(&mut self) -> Vec<u32> {
        // Keep exited group leaders until stop, so descendant processes are still killed.
        self.apps.iter().map(|app| app.pgid as u32).collect()
    }
    async fn accessibility(
        &mut self,
        action: &str,
        ref_id: Option<&str>,
        text: Option<&str>,
    ) -> Result<Value> {
        ensure!(
            self.registry
                .as_mut()
                .is_some_and(|p| p.try_wait().is_ok_and(|s| s.is_none())),
            "Private AT-SPI registry unavailable; install at-spi2-core or use screenshot coordinates"
        );
        let pids = self.pids();
        if self.bridge.is_none() {
            let mut child = ManagedChild::new(
                command(
                    binary("python3").context("Python 3 unavailable")?,
                    &self.env,
                )
                .arg("-I")
                .arg("-c")
                .arg(HELPER)
                .arg("bridge")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()?,
            );
            let input = child.stdin.take().context("Helper stdin")?;
            let output = BufReader::new(child.stdout.take().context("Helper stdout")?);
            self.bridge = Some(Bridge {
                child,
                input,
                output,
            });
        }
        let bridge = self.bridge.as_mut().unwrap();
        let request =
            serde_json::to_vec(&json!({"action":action,"ref_id":ref_id,"text":text,"pids":pids}))?;
        let result=tokio::time::timeout(Duration::from_secs(2),async{
            bridge.input.write_all(&request).await?;bridge.input.write_all(b"\n").await?;bridge.input.flush().await?;
            let mut line=String::new();
            // Helper output is fixed and capped to 512 nodes, but enforce a hard transport cap too.
            let count=bridge.output.read_line(&mut line).await?;
            ensure!(count>0&&count<=2*1024*1024,"Native accessibility helper exited or returned invalid output; install Python GI Atspi");
            let response:Value=serde_json::from_str(&line)?;
            ensure!(response["ok"]==true,"Native accessibility: {}",response["error"].as_str().unwrap_or("request failed"));
            Ok::<Value,anyhow::Error>(response["result"].clone())
        }).await;
        match result {
            Ok(result) => result,
            Err(_) => {
                if let Some(mut bridge) = self.bridge.take() {
                    terminate(&mut bridge.child);
                    let _ = bridge.child.wait().await;
                }
                bail!(
                    "Native accessibility timed out; references invalidated, observe again or use private screenshot coordinates"
                )
            }
        }
    }
    async fn execute(&mut self, action: &ComputerAction) -> Result<ComputerOutput> {
        ensure!(
            self.compositor.try_wait()?.is_none(),
            "Private compositor has exited; stop the lease and retry"
        );
        match action.action.as_str() {
            "launch" | "open" => {
                let argv = action
                    .argv
                    .as_ref()
                    .context("Native launch requires argv, not a shell command or URL")?;
                ensure!(
                    !argv.is_empty()
                        && argv.len() <= 64
                        && argv.iter().all(|s| s.len() <= 8192 && !s.contains('\0')),
                    "Invalid launch argv"
                );
                ensure!(
                    self.apps.len() < 8,
                    "Private desktop application limit is 8; stop this lease to release apps"
                );
                let executable = if Path::new(&argv[0]).is_absolute() {
                    PathBuf::from(&argv[0])
                } else {
                    ensure!(
                        !argv[0].contains('/'),
                        "Use an absolute executable path or executable name"
                    );
                    binary(&argv[0])
                        .context("Application executable not found in fixed safe PATH")?
                };
                let child = ManagedChild::new(
                    command(executable, &self.env)
                        .args(&argv[1..])
                        .current_dir(self.root.join("home"))
                        .spawn()
                        .context("Launch private native application")?,
                );
                let pid = child.id();
                self.apps.push(child);
                Ok(text_output(json!({"ok":true,"launched":argv[0],"pid":pid,"isolation":"private desktop with isolated empty HOME (does NOT contain host app logins); use handoff if user login/2FA is needed"}).to_string()))
            }
            "wait" => {
                let ms = action.delta.unwrap_or(500).clamp(1, 30_000) as u64;
                tokio::time::sleep(Duration::from_millis(ms)).await;
                Ok(text_output(
                    json!({"ok":true,"action":"wait","target":"desktop","waited_ms":ms}).to_string(),
                ))
            }
            "observe" => Ok(text_output(
                self.accessibility("observe", None, None).await?.to_string(),
            )),
            "fill" => {
                let text = action.text.as_deref().context("fill requires text")?;
                ensure!(text.len() <= 65536, "Text exceeds limit");
                if let Some(reference) = action.ref_id.as_deref() {
                    return Ok(text_output(
                        self.accessibility("fill", Some(reference), Some(text))
                            .await?
                            .to_string(),
                    ));
                }
                let x = action.x.context("Desktop fill requires ref_id or (x, y) coordinates")?;
                let y = action.y.context("Desktop fill requires ref_id or (x, y) coordinates")?;
                let click_action = ComputerAction {
                    action: "click".into(), target: action.target.clone(), ref_id: None, url: None,
                    text: None, x: Some(x), y: Some(y), key: None, button: Some("left".into()),
                    delta: None, argv: None, max_nodes: None, max_depth: None, color: None,
                };
                let type_action = ComputerAction {
                    action: "type".into(), target: action.target.clone(), ref_id: None, url: None,
                    text: Some(text.to_owned()), x: None, y: None, key: None, button: None,
                    delta: None, argv: None, max_nodes: None, max_depth: None, color: None,
                };
                let mut cmds = input_commands(&click_action)?;
                cmds.extend(input_commands(&type_action)?);
                let socket = self.root.join(&self.display);
                tokio::task::spawn_blocking(move || send_input(&socket, cmds)).await??;
                Ok(text_output(json!({"ok":true,"action":"fill","target":"desktop","x":x,"y":y,"text_len":text.len()}).to_string()))
            }
            "click" if action.ref_id.is_some() => Ok(text_output(
                self.accessibility("click", action.ref_id.as_deref(), None)
                    .await?
                    .to_string(),
            )),
            "type" if action.ref_id.is_some() => {
                let text = action.text.as_deref().context("type requires text")?;
                ensure!(text.len() <= 65536, "Text exceeds limit");
                Ok(text_output(
                    self.accessibility("type", action.ref_id.as_deref(), Some(text))
                        .await?
                        .to_string(),
                ))
            }
            "screenshot" => {
                let image = self.screenshot().await?;
                self.last_capture = Some(Instant::now());
                Ok(ComputerOutput {
                    text: format!(
                        "Private desktop screenshot {}x{}; coordinates are logical pixels",
                        image.width, image.height
                    ),
                    image: Some(image),
                })
            }
            "ocr" => {
                let image = self.screenshot().await?;
                self.last_capture = Some(Instant::now());
                let ocr = run_vision(&image.png, &["ocr"]).await?;
                Ok(text_output(ocr.to_string()))
            }
            "find_color" => {
                let color = action.color.as_deref().context("find_color requires color (#RRGGBB or R,G,B)")?;
                let tol = action.delta.unwrap_or(24).clamp(0, 255).to_string();
                let image = self.screenshot().await?;
                self.last_capture = Some(Instant::now());
                let found = run_vision(&image.png, &["find_color", color, &tol]).await?;
                Ok(text_output(found.to_string()))
            }
            "click_text" => {
                let query = action.text.as_deref().filter(|s| !s.trim().is_empty()).context("click_text requires text")?;
                let before = self.screenshot().await?;
                self.last_capture = Some(Instant::now());
                let ocr = run_vision(&before.png, &["ocr"]).await?;
                let (matched, cx, cy, bbox) = pick_ocr_match(&ocr, query)?;
                let click_action = ComputerAction {
                    action: "click".into(), target: action.target.clone(), ref_id: None, url: None,
                    text: None, x: Some(cx), y: Some(cy), key: None, button: action.button.clone(),
                    delta: None, argv: None, max_nodes: None, max_depth: None, color: None,
                };
                let cmds = input_commands(&click_action)?;
                let socket = self.root.join(&self.display);
                tokio::task::spawn_blocking(move || send_input(&socket, cmds)).await??;
                let (screen_changed, changed_percent, after) =
                    self.verify_input_after(Some(&before), Some((cx, cy))).await;
                Ok(ComputerOutput {
                    text: json!({
                        "ok": true,
                        "action": "click_text",
                        "target": "desktop",
                        "matched_text": matched,
                        "cx": cx,
                        "cy": cy,
                        "bbox": bbox,
                        "screen_changed": screen_changed,
                        "changed_percent": changed_percent,
                        "note": format!("Clicked ({cx}, {cy}); screenshot marks click target with a red crosshair")
                    }).to_string(),
                    image: after,
                })
            }
            "click_color" => {
                let color = action.color.as_deref().context("click_color requires color (#RRGGBB or R,G,B)")?;
                let tol = action.delta.unwrap_or(24).clamp(0, 255).to_string();
                let before = self.screenshot().await?;
                self.last_capture = Some(Instant::now());
                let found = run_vision(&before.png, &["find_color", color, &tol]).await?;
                let cluster = found["clusters"].as_array().and_then(|a| a.first()).context("No matching color cluster found on screen")?;
                let cx = cluster["cx"].as_i64().context("Missing cluster cx")? as i32;
                let cy = cluster["cy"].as_i64().context("Missing cluster cy")? as i32;
                let click_action = ComputerAction {
                    action: "click".into(), target: action.target.clone(), ref_id: None, url: None,
                    text: None, x: Some(cx), y: Some(cy), key: None, button: action.button.clone(),
                    delta: None, argv: None, max_nodes: None, max_depth: None, color: None,
                };
                let cmds = input_commands(&click_action)?;
                let socket = self.root.join(&self.display);
                tokio::task::spawn_blocking(move || send_input(&socket, cmds)).await??;
                let (screen_changed, changed_percent, after) =
                    self.verify_input_after(Some(&before), Some((cx, cy))).await;
                Ok(ComputerOutput {
                    text: json!({
                        "ok": true,
                        "action": "click_color",
                        "target": "desktop",
                        "color": color,
                        "cx": cx,
                        "cy": cy,
                        "cluster": cluster,
                        "screen_changed": screen_changed,
                        "changed_percent": changed_percent,
                        "note": format!("Clicked ({cx}, {cy}); screenshot marks click target with a red crosshair")
                    }).to_string(),
                    image: after,
                })
            }
            "handoff" => {
                self.handoff.take();
                let host_env = self.host_env.clone().context("Host desktop display is unavailable for handoff viewer")?;
                let python = binary("python3").context("python3 required for handoff viewer")?;
                let frame_path = self.root.join("handoff_frame.png");
                let input_path = self.root.join("handoff_input.jsonl");
                let _ = tokio::fs::remove_file(&input_path).await;
                if let Ok(img) = self.screenshot().await {
                    let _ = tokio::fs::write(&frame_path, &img.png).await;
                }
                let mut viewer_cmd = Command::new(&python);
                viewer_cmd
                    .env_clear()
                    .env("PATH", "/usr/local/bin:/usr/bin:/bin")
                    .env("DBUS_SESSION_BUS_ADDRESS", &host_env.bus)
                    .env("DBUS_STARTER_ADDRESS", &host_env.bus);
                if let Some(dir) = host_env.socket.parent() {
                    viewer_cmd.env("XDG_RUNTIME_DIR", dir);
                }
                if let Some(name) = host_env.socket.file_name() {
                    viewer_cmd.env("WAYLAND_DISPLAY", name);
                }
                viewer_cmd
                    .arg("-I")
                    .arg("-c")
                    .arg(HANDOFF_HELPER)
                    .arg(&frame_path)
                    .arg(&input_path)
                    .arg(WIDTH.to_string())
                    .arg(HEIGHT.to_string())
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .kill_on_drop(true)
                    .process_group(0);
                let viewer = ManagedChild::new(viewer_cmd.spawn().context("Launch GTK3 handoff viewer on host")?);
                let stop = CancellationToken::new();
                let stop_bg = stop.clone();
                let bus = self.bus.clone();
                let socket = self.root.join(&self.display);
                let task = tokio::spawn(async move {
                    let mut read_offset = 0u64;
                    while !stop_bg.is_cancelled() {
                        if let Ok(img) = capture_workspace(&bus).await {
                            let tmp = frame_path.with_extension("tmp");
                            if tokio::fs::write(&tmp, &img.png).await.is_ok() {
                                let _ = tokio::fs::rename(&tmp, &frame_path).await;
                            }
                        }
                        if let Ok(mut file) = tokio::fs::File::open(&input_path).await
                            && file.seek(SeekFrom::Start(read_offset)).await.is_ok()
                        {
                            use tokio::io::AsyncReadExt;
                            let mut buf = String::new();
                            if let Ok(n) = file.read_to_string(&mut buf).await
                                && n > 0
                            {
                                read_offset += n as u64;
                                for line in buf.lines() {
                                    if let Ok(ev) = serde_json::from_str::<Value>(line) {
                                        let act = ComputerAction {
                                            action: ev["action"].as_str().unwrap_or("").to_owned(),
                                            target: Some("desktop".into()),
                                            ref_id: None, url: None,
                                            text: ev["text"].as_str().map(str::to_owned),
                                            x: ev["x"].as_i64().map(|v| v as i32),
                                            y: ev["y"].as_i64().map(|v| v as i32),
                                            key: ev["key"].as_str().map(str::to_owned),
                                            button: ev["button"].as_str().map(str::to_owned),
                                            delta: ev["delta"].as_i64().map(|v| v as i32),
                                            argv: None, max_nodes: None, max_depth: None, color: None,
                                        };
                                        if let Ok(cmds) = input_commands(&act) {
                                            let sock = socket.clone();
                                            let _ = tokio::task::spawn_blocking(move || send_input(&sock, cmds)).await;
                                        }
                                    }
                                }
                            }
                        }
                        tokio::select! {
                            _ = stop_bg.cancelled() => break,
                            _ = tokio::time::sleep(Duration::from_millis(160)) => {}
                        }
                    }
                });
                self.handoff = Some(DesktopHandoff { stop, viewer, task });
                Ok(text_output(json!({"ok":true,"action":"handoff","target":"desktop","message":"Interactive private desktop window opened on user's screen. Call ask_user(blocking=true) now so the user can complete login/2FA and click Continue, then call computer(target='desktop', action='resume_headless') to close the window and resume."}).to_string()))
            }
            "resume_headless" => {
                self.handoff.take();
                let image = self.screenshot().await?;
                self.last_capture = Some(Instant::now());
                Ok(ComputerOutput {
                    text: format!(
                        "Private desktop handoff closed and resumed ({}x{}). Logged-in state preserved.",
                        image.width, image.height
                    ),
                    image: Some(image),
                })
            }
            "click" | "double_click" | "right_click" | "scroll" | "key" | "type" => {
                if matches!(action.action.as_str(), "click" | "double_click" | "right_click" | "scroll")
                    && (action.x.is_some() || action.y.is_some())
                    || matches!(action.action.as_str(), "click" | "double_click" | "right_click")
                {
                    ensure!(
                        self.last_capture
                            .is_some_and(|t| t.elapsed() < Duration::from_secs(120)),
                        "Take a fresh private screenshot/ocr before coordinate input"
                    );
                }
                let before = self.screenshot().await.ok();
                let commands = input_commands(action)?;
                let socket = self.root.join(&self.display);
                tokio::task::spawn_blocking(move || send_input(&socket, commands)).await??;
                let target_xy = match (action.x, action.y) {
                    (Some(x), Some(y)) => Some((x, y)),
                    _ => None,
                };
                let (screen_changed, changed_percent, after) =
                    self.verify_input_after(before.as_ref(), target_xy).await;
                let note = if let Some((x, y)) = target_xy {
                    format!("Input sent to private virtual compositor; screenshot marks ({x}, {y}) with a red crosshair")
                } else {
                    "Input sent to private virtual compositor".into()
                };
                Ok(ComputerOutput {
                    text: json!({
                        "ok": true,
                        "action": action.action,
                        "target": "desktop",
                        "x": action.x,
                        "y": action.y,
                        "key": action.key,
                        "delta": action.delta,
                        "screen_changed": screen_changed,
                        "changed_percent": changed_percent,
                        "note": note
                    })
                    .to_string(),
                    image: after,
                })
            }
            _ => bail!("Unsupported private desktop action"),
        }
    }
    async fn screenshot(&self) -> Result<ComputerImage> {
        capture_workspace(&self.bus).await.map_err(|e| {
            let mut log = String::new();
            if let Ok(file) = File::open(self.root.join("kwin.log")) {
                let _ = file.take(8192).read_to_string(&mut log);
            }
            e.context(format!(
                "Private KWin capture failed; compositor diagnostics: {log}"
            ))
        })
    }
    async fn verify_input_after(
        &mut self,
        before: Option<&ComputerImage>,
        target_xy: Option<(i32, i32)>,
    ) -> (bool, f64, Option<ComputerImage>) {
        tokio::time::sleep(Duration::from_millis(140)).await;
        let Ok(after) = self.screenshot().await else {
            return (false, 0.0, None);
        };
        self.last_capture = Some(Instant::now());
        let changed_percent = before
            .map(|b| pixel_diff_percent(b, &after))
            .unwrap_or(0.0);
        let screen_changed = changed_percent >= 0.05;
        let out_img = match target_xy {
            Some((x, y)) => annotate_crosshair(&after, x, y).unwrap_or(after),
            None => after,
        };
        (screen_changed, changed_percent, Some(out_img))
    }
}

fn pixel_diff_percent(a: &ComputerImage, b: &ComputerImage) -> f64 {
    let (Ok(ia), Ok(ib)) = (
        image::load_from_memory(&a.png),
        image::load_from_memory(&b.png),
    ) else {
        return 0.0;
    };
    let (ra, rb) = (ia.to_rgba8(), ib.to_rgba8());
    let (ba, bb) = (ra.as_raw(), rb.as_raw());
    if ba.len() != bb.len() || ba.is_empty() {
        return 0.0;
    }
    let mut changed = 0usize;
    let total = ba.len() / 4;
    for (pa, pb) in ba.chunks_exact(4).zip(bb.chunks_exact(4)) {
        let diff = (pa[0] as i32 - pb[0] as i32).abs()
            + (pa[1] as i32 - pb[1] as i32).abs()
            + (pa[2] as i32 - pb[2] as i32).abs();
        if diff > 24 {
            changed += 1;
        }
    }
    ((changed as f64 * 100.0 / total as f64) * 100.0).round() / 100.0
}

fn annotate_crosshair(img: &ComputerImage, cx: i32, cy: i32) -> Result<ComputerImage> {
    let mut rgba = image::load_from_memory(&img.png)?.to_rgba8();
    let w = rgba.width() as i32;
    let h = rgba.height() as i32;
    let red = image::Rgba([255, 40, 40, 255]);
    let white = image::Rgba([255, 255, 255, 255]);
    for d in -14..=14 {
        for off in -1..=1 {
            if (0..w).contains(&(cx + d)) && (0..h).contains(&(cy + off)) {
                rgba.put_pixel((cx + d) as u32, (cy + off) as u32, if off == 0 { red } else { white });
            }
            if (0..w).contains(&(cx + off)) && (0..h).contains(&(cy + d)) {
                rgba.put_pixel((cx + off) as u32, (cy + d) as u32, if off == 0 { red } else { white });
            }
        }
    }
    for angle in 0..72 {
        let theta = (angle as f64) * std::f64::consts::TAU / 72.0;
        let px = cx + (10.0 * theta.cos()).round() as i32;
        let py = cy + (10.0 * theta.sin()).round() as i32;
        if (0..w).contains(&px) && (0..h).contains(&py) {
            rgba.put_pixel(px as u32, py as u32, red);
        }
    }
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png).write_image(
        rgba.as_raw(),
        rgba.width(),
        rgba.height(),
        image::ExtendedColorType::Rgba8,
    )?;
    Ok(ComputerImage {
        png,
        width: img.width,
        height: img.height,
    })
}
async fn capture_workspace(bus: &zbus::Connection) -> Result<ComputerImage> {
    let fd = nix::sys::memfd::memfd_create(
        "celoom-private-capture",
        nix::sys::memfd::MFdFlags::MFD_CLOEXEC,
    )?;
    let mut file = File::from(fd);
    let proxy = zbus::Proxy::new(
        bus,
        "org.kde.KWin",
        "/org/kde/KWin/ScreenShot2",
        "org.kde.KWin.ScreenShot2",
    )
    .await?;
    let options = HashMap::<String, zbus::zvariant::Value<'_>>::new();
    let metadata: HashMap<String, zbus::zvariant::OwnedValue> = proxy
        .call(
            "CaptureWorkspace",
            &(options, zbus::zvariant::Fd::from(file.as_fd())),
        )
        .await
        .context("Capture private KWin workspace (ScreenShot2 required)")?;
    let number = |name: &str| -> Result<u32> {
        u32::try_from(
            metadata
                .get(name)
                .with_context(|| format!("Screenshot missing {name}"))?,
        )
        .with_context(|| format!("Invalid screenshot {name}"))
    };
    let width = number("width")?;
    let height = number("height")?;
    let stride = number("stride")?;
    let format = number("format")?;
    let required = validate_layout(width, height, stride, format)?;
    // KWin may reply with metadata before its asynchronous FD writer finishes.
    let start = Instant::now();
    while file.metadata()?.len() < required as u64 {
        ensure!(
            start.elapsed() < Duration::from_secs(5),
            "Private screenshot pixel writer timed out"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    file.seek(SeekFrom::Start(0))?;
    let mut raw = vec![0; required];
    file.read_exact(&mut raw)?;
    encode_capture(width, height, stride, format, &raw)
}

async fn run_vision(png: &[u8], args: &[&str]) -> Result<Value> {
    let python = binary("python3").context("python3 required for native OCR/color vision")?;
    let tmp = std::env::temp_dir().join(format!("celoom-vision-{}.png", uuid::Uuid::new_v4()));
    tokio::fs::write(&tmp, png).await?;
    let out = Command::new(python)
        .arg("-I")
        .arg("-c")
        .arg(VISION_HELPER)
        .args(args.iter().take(1))
        .arg(&tmp)
        .args(args.iter().skip(1))
        .stdin(Stdio::null())
        .output()
        .await;
    let _ = tokio::fs::remove_file(&tmp).await;
    let out = out.context("Execute native vision helper")?;
    ensure!(
        out.status.success(),
        "Native vision helper failed: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    serde_json::from_slice(&out.stdout).context("Invalid JSON from native vision helper")
}

fn find_in_ocr<'a>(arr: &'a [Value], q: &str) -> Option<&'a Value> {
    arr.iter()
        .find(|it| it["text"].as_str().is_some_and(|t| t.trim().to_ascii_lowercase() == q))
        .or_else(|| {
            arr.iter()
                .find(|it| it["text"].as_str().is_some_and(|t| t.to_ascii_lowercase().contains(q)))
        })
}

fn pick_ocr_match(ocr: &Value, query: &str) -> Result<(String, i32, i32, Value)> {
    let q = query.trim().to_ascii_lowercase();
    let lines = ocr["lines"].as_array();
    let items = ocr["items"].as_array().context("OCR returned no items")?;
    let best = lines
        .and_then(|arr| find_in_ocr(arr, &q))
        .or_else(|| find_in_ocr(items, &q));
    let Some(item) = best else {
        let preview: Vec<_> = lines
            .map(|arr| {
                arr.iter()
                    .filter_map(|it| it["text"].as_str())
                    .take(20)
                    .collect::<Vec<_>>()
            })
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| items.iter().filter_map(|it| it["text"].as_str()).take(20).collect());
        bail!(
            "OCR could not find text '{}'; visible OCR lines include: {:?}",
            query,
            preview
        );
    };
    let text = item["text"].as_str().unwrap_or("").to_owned();
    let cx = item["cx"].as_i64().context("OCR missing cx")? as i32;
    let cy = item["cy"].as_i64().context("OCR missing cy")? as i32;
    let bbox = json!([item["x"], item["y"], item["w"], item["h"]]);
    Ok((text, cx, cy, bbox))
}

fn validate_layout(width: u32, height: u32, stride: u32, format: u32) -> Result<usize> {
    ensure!(
        width > 0 && height > 0 && width <= 8192 && height <= 8192,
        "Invalid screenshot dimensions"
    );
    ensure!(
        matches!(format, 5 | 16 | 17 | 18),
        "Unsupported Qt screenshot pixel format {format}"
    );
    ensure!(
        stride >= width.checked_mul(4).context("Screenshot width overflow")?,
        "Invalid screenshot stride"
    );
    let size = (stride as usize)
        .checked_mul(height as usize)
        .context("Screenshot size overflow")?;
    ensure!(size <= MAX_IMAGE, "Screenshot exceeds 256 MiB limit");
    Ok(size)
}
fn encode_capture(
    width: u32,
    height: u32,
    stride: u32,
    format: u32,
    raw: &[u8],
) -> Result<ComputerImage> {
    let size = validate_layout(width, height, stride, format)?;
    ensure!(raw.len() >= size, "Truncated screenshot pixels");
    let mut rgba = vec![0u8; width as usize * height as usize * 4];
    for y in 0..height as usize {
        for x in 0..width as usize {
            let src = &raw[y * stride as usize + x * 4..][..4];
            let dst = &mut rgba[(y * width as usize + x) * 4..][..4];
            if format == 5 {
                dst.copy_from_slice(&[src[2], src[1], src[0], src[3]]);
            } else {
                dst.copy_from_slice(src);
            }
            if format == 16 {
                dst[3] = 255;
            }
            if format == 18 && dst[3] > 0 {
                let a = dst[3] as u32;
                for c in &mut dst[..3] {
                    *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
                }
            }
        }
    }
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png).write_image(
        &rgba,
        width,
        height,
        image::ExtendedColorType::Rgba8,
    )?;
    ensure!(
        png.len() <= 16 * 1024 * 1024,
        "Encoded screenshot exceeds 16 MiB limit"
    );
    Ok(ComputerImage { png, width, height })
}

#[derive(Clone, Debug)]
enum Input {
    Move(f64, f64),
    Button(u32, u32),
    Axis(f64),
    Key(u32, u32),
    Keysym(u32, u32),
}
fn keycode(key: &str) -> Option<u32> {
    Some(match key.to_ascii_lowercase().as_str() {
        "escape" | "esc" => 1,
        "enter" | "return" => 28,
        "tab" => 15,
        "backspace" => 14,
        "delete" => 111,
        "insert" => 110,
        "left" | "arrowleft" => 105,
        "right" | "arrowright" => 106,
        "up" | "arrowup" => 103,
        "down" | "arrowdown" => 108,
        "home" => 102,
        "end" => 107,
        "pageup" => 104,
        "pagedown" => 109,
        "space" => 57,
        "ctrl" | "control" => 29,
        "shift" => 42,
        "alt" => 56,
        "super" | "meta" => 125,
        "a" => 30,
        "b" => 48,
        "c" => 46,
        "d" => 32,
        "e" => 18,
        "f" => 33,
        "g" => 34,
        "h" => 35,
        "i" => 23,
        "j" => 36,
        "k" => 37,
        "l" => 38,
        "m" => 50,
        "n" => 49,
        "o" => 24,
        "p" => 25,
        "q" => 16,
        "r" => 19,
        "s" => 31,
        "t" => 20,
        "u" => 22,
        "v" => 47,
        "w" => 17,
        "x" => 45,
        "y" => 21,
        "z" => 44,
        "1" => 2,
        "2" => 3,
        "3" => 4,
        "4" => 5,
        "5" => 6,
        "6" => 7,
        "7" => 8,
        "8" => 9,
        "9" => 10,
        "0" => 11,
        "f1" => 59,
        "f2" => 60,
        "f3" => 61,
        "f4" => 62,
        "f5" => 63,
        "f6" => 64,
        "f7" => 65,
        "f8" => 66,
        "f9" => 67,
        "f10" => 68,
        "f11" => 87,
        "f12" => 88,
        _ => return None,
    })
}
fn input_commands(action: &ComputerAction) -> Result<Vec<Input>> {
    input_commands_with_geometry(action, WIDTH, HEIGHT)
}
fn input_commands_with_geometry(
    action: &ComputerAction,
    width: u32,
    height: u32,
) -> Result<Vec<Input>> {
    let mut commands = Vec::new();
    if action.x.is_some() || action.y.is_some() {
        let x = action.x.context("Both x and y are required")?;
        let y = action.y.context("Both x and y are required")?;
        ensure!(
            x >= 0 && y >= 0 && (x as u32) < width && (y as u32) < height,
            "Coordinates outside captured desktop geometry"
        );
        commands.push(Input::Move(x as f64, y as f64));
    }
    match action.action.as_str() {
        "click" | "right_click" | "double_click" => {
            ensure!(
                action.x.is_some() && action.y.is_some(),
                "Coordinate click requires x and y or accessibility ref_id"
            );
            let button = if action.action == "right_click" {
                273
            } else {
                match action.button.as_deref().unwrap_or("left") {
                    "left" => 272,
                    "right" => 273,
                    "middle" => 274,
                    _ => bail!("Unsupported mouse button"),
                }
            };
            commands.push(Input::Button(button, 1));
            commands.push(Input::Button(button, 0));
            if action.action == "double_click" {
                commands.push(Input::Button(button, 1));
                commands.push(Input::Button(button, 0));
            }
        }
        "scroll" => {
            let delta = action.delta.context("scroll requires delta")?;
            ensure!(delta.unsigned_abs() <= 10000, "Scroll exceeds limit");
            commands.push(Input::Axis(delta as f64));
        }
        "key" => {
            let key = action.key.as_deref().context("key requires key name")?;
            let pieces: Vec<_> = key.split('+').collect();
            ensure!(!pieces.is_empty() && pieces.len() <= 5, "Invalid key chord");
            let codes: Vec<_> = pieces
                .iter()
                .map(|s| keycode(s).with_context(|| format!("Unsupported key {s}")))
                .collect::<Result<_>>()?;
            for code in &codes {
                commands.push(Input::Key(*code, 1));
            }
            for code in codes.iter().rev() {
                commands.push(Input::Key(*code, 0));
            }
        }
        "type" => {
            let text = action.text.as_deref().context("type requires text")?;
            ensure!(text.len() <= 65536, "Text exceeds limit");
            for ch in text.chars() {
                let sym = match ch {
                    '\n' => 0xff0d,
                    '\t' => 0xff09,
                    c if c as u32 >= 0x20 && (c as u32) <= 0xff => c as u32,
                    c if !c.is_control() => 0x01000000 | c as u32,
                    _ => bail!("Control characters are not accepted in native type"),
                };
                commands.push(Input::Keysym(sym, 1));
                commands.push(Input::Keysym(sym, 0));
            }
        }
        _ => bail!("Unsupported input operation"),
    }
    Ok(commands)
}
#[derive(Default, Clone, Debug)]
struct OutputGeometry {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    scale: i32,
    normal: bool,
}
struct InputState {
    fake: Option<OrgKdeKwinFakeInput>,
    outputs: HashMap<u32, OutputGeometry>,
    synced: bool,
}
fn validate_host_outputs(
    outputs: &HashMap<u32, OutputGeometry>,
    geometry: Option<(u32, u32)>,
) -> Result<()> {
    ensure!(
        outputs.len() == 1,
        "COORDINATE_MAPPING_UNSUPPORTED: host requires one output, scale1 and origin0,0"
    );
    let output = outputs.values().next().unwrap();
    ensure!(
        output.x == 0
            && output.y == 0
            && output.scale == 1
            && output.normal
            && output.width > 0
            && output.height > 0,
        "COORDINATE_MAPPING_UNSUPPORTED: scaled, rotated or offset host output"
    );
    ensure!(
        geometry == Some((output.width as u32, output.height as u32)),
        "COORDINATE_MAPPING_UNSUPPORTED: screenshot geometry differs from current single-output mode"
    );
    Ok(())
}
impl Dispatch<wl_output::WlOutput, u32> for InputState {
    fn event(
        state: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        name: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let output = state.outputs.entry(*name).or_default();
        match event {
            wl_output::Event::Geometry {
                x, y, transform, ..
            } => {
                output.x = x;
                output.y = y;
                output.normal = matches!(
                    transform,
                    wayland_client::WEnum::Value(wl_output::Transform::Normal)
                );
            }
            wl_output::Event::Scale { factor } => output.scale = factor,
            wl_output::Event::Mode {
                flags,
                width,
                height,
                ..
            } => {
                if matches!(flags,wayland_client::WEnum::Value(flags) if flags.contains(wl_output::Mode::Current))
                {
                    output.width = width;
                    output.height = height;
                }
            }
            _ => {}
        }
    }
}
impl Dispatch<wl_callback::WlCallback, ()> for InputState {
    fn event(
        state: &mut Self,
        _: &wl_callback::WlCallback,
        _: wl_callback::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        state.synced = true;
    }
}
fn bounded_roundtrip(
    connection: &Connection,
    queue: &mut wayland_client::EventQueue<InputState>,
    state: &mut InputState,
) -> Result<()> {
    state.synced = false;
    connection.display().sync(&queue.handle(), ());
    connection.flush()?;
    let deadline = Instant::now() + Duration::from_secs(2);
    while !state.synced {
        queue.dispatch_pending(state)?;
        if state.synced {
            break;
        }
        ensure!(Instant::now() < deadline, "Wayland roundtrip timed out");
        if let Some(guard) = queue.prepare_read() {
            match guard.read() {
                Ok(_) => {}
                Err(wayland_client::backend::WaylandError::Io(e))
                    if e.kind() == std::io::ErrorKind::WouldBlock =>
                {
                    std::thread::park_timeout(Duration::from_millis(2))
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
    Ok(())
}
impl Dispatch<wl_registry::WlRegistry, ()> for InputState {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            if interface == "org_kde_kwin_fake_input" && version >= 4 {
                state.fake = Some(registry.bind(name, version.min(6), qh, ()));
            } else if interface == "wl_output" {
                let _: wl_output::WlOutput = registry.bind(name, version.min(4), qh, name);
                state.outputs.insert(
                    name,
                    OutputGeometry {
                        scale: 1,
                        ..Default::default()
                    },
                );
            }
        }
    }
}
impl Dispatch<OrgKdeKwinFakeInput, ()> for InputState {
    fn event(
        _: &mut Self,
        _: &OrgKdeKwinFakeInput,
        _: crate::computer_fake_input::org_kde_kwin_fake_input::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
fn send_input(socket: &Path, commands: Vec<Input>) -> Result<()> {
    send_input_scoped(
        socket,
        commands,
        false,
        CancellationToken::new(),
        CancellationToken::new(),
        None,
    )
}
fn send_input_scoped(
    socket: &Path,
    commands: Vec<Input>,
    host: bool,
    cancel: CancellationToken,
    emergency: CancellationToken,
    geometry: Option<(u32, u32)>,
) -> Result<()> {
    ensure!(
        host || socket
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|s| s.to_string_lossy().starts_with("celoom-desktop-")),
        "Input must use private runtime socket"
    );
    let stream = UnixStream::connect(socket).context("Connect private Wayland input")?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let connection = Connection::from_socket(stream)?;
    let mut queue = connection.new_event_queue();
    let qh = queue.handle();
    let _registry = connection.display().get_registry(&qh, ());
    let mut state = InputState {
        fake: None,
        outputs: HashMap::new(),
        synced: false,
    };
    bounded_roundtrip(&connection, &mut queue, &mut state)?;
    if host && commands.iter().any(|c| matches!(c, Input::Move(..))) {
        // Registry binds occur during the first dispatch; a second sync gets all
        // initial wl_output mode/scale/geometry events before any host injection.
        bounded_roundtrip(&connection, &mut queue, &mut state)?;
        validate_host_outputs(&state.outputs, geometry)?;
    }
    let fake = state
        .fake
        .clone()
        .context("Private KWin does not expose fake_input v4")?;
    fake.authenticate(
        "Celoom private desktop".into(),
        "Input exclusively inside self-owned virtual compositor".into(),
    );
    connection.flush()?;
    bounded_roundtrip(&connection, &mut queue, &mut state)?;
    ensure!(
        !commands.iter().any(|c| matches!(c, Input::Keysym(..))) || fake.version() >= 6,
        "Native type requires fake_input v6; use accessibility fill on older KWin"
    );
    let mut keys = HashSet::new();
    let mut symbols = HashSet::new();
    let mut buttons = HashSet::new();
    let start = Instant::now();
    for command in commands {
        if cancel.is_cancelled()
            || emergency.is_cancelled()
            || start.elapsed() > Duration::from_secs(20)
        {
            break;
        }
        match command {
            Input::Move(x, y) => {
                fake.pointer_motion_absolute(x, y);
                connection.flush()?;
                std::thread::sleep(Duration::from_millis(20));
            }
            Input::Button(button, value) => {
                fake.button(button, value);
                if value == 1 {
                    buttons.insert(button);
                } else {
                    buttons.remove(&button);
                }
                connection.flush()?;
                std::thread::sleep(Duration::from_millis(20));
            }
            Input::Axis(value) => {
                fake.axis(0, value);
                connection.flush()?;
                std::thread::sleep(Duration::from_millis(10));
            }
            Input::Key(key, value) => {
                fake.keyboard_key(key, value);
                if value == 1 {
                    keys.insert(key);
                } else {
                    keys.remove(&key);
                }
                connection.flush()?;
                std::thread::sleep(Duration::from_millis(10));
            }
            Input::Keysym(key, value) => {
                fake.keyboard_keysym(key, value);
                if value == 1 {
                    symbols.insert(key);
                } else {
                    symbols.remove(&key);
                }
                connection.flush()?;
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
    for key in keys {
        fake.keyboard_key(key, 0);
    }
    for symbol in symbols {
        fake.keyboard_keysym(symbol, 0);
    }
    for button in buttons {
        fake.button(button, 0);
    }
    // Releases are queued even if main cancelled between a press and its release.
    if fake.version() >= 5 {
        fake.destroy();
    }
    connection.flush()?;
    bounded_roundtrip(&connection, &mut queue, &mut state)?;
    ensure!(
        !cancel.is_cancelled() && !emergency.is_cancelled(),
        "Input cancelled; held keys/buttons released"
    );
    ensure!(
        start.elapsed() <= Duration::from_secs(20),
        "Input deadline exceeded; partial text may have been entered"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn action(name: &str) -> ComputerAction {
        ComputerAction {
            action: name.into(),
            target: Some("desktop".into()),
            ref_id: None,
            url: None,
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
        }
    }
    #[test]
    fn screenshot_formats_and_padding() {
        let image = encode_capture(1, 1, 8, 5, &[3, 2, 1, 255, 0, 0, 0, 0]).unwrap();
        let decoded = image::load_from_memory(&image.png).unwrap().to_rgba8();
        assert_eq!(decoded.as_raw(), &[1, 2, 3, 255]);
        let image = encode_capture(1, 1, 4, 18, &[64, 0, 0, 128]).unwrap();
        let decoded = image::load_from_memory(&image.png).unwrap().to_rgba8();
        assert_eq!(decoded.as_raw(), &[128, 0, 0, 128]);
        assert!(validate_layout(u32::MAX, 1, 4, 16).is_err());
        assert!(validate_layout(10, 10, 1, 16).is_err());
        assert!(validate_layout(1, 1, 4, 99).is_err());
    }
    #[test]
    fn host_coordinates_fail_closed_without_single_unscaled_output() {
        let mut outputs = HashMap::new();
        assert!(validate_host_outputs(&outputs, Some((1280, 800))).is_err());
        let normal = OutputGeometry {
            x: 0,
            y: 0,
            width: 1280,
            height: 800,
            scale: 1,
            normal: true,
        };
        outputs.insert(1, normal.clone());
        assert!(validate_host_outputs(&outputs, Some((1280, 800))).is_ok());
        assert!(validate_host_outputs(&outputs, Some((640, 400))).is_err());
        outputs.insert(2, normal.clone());
        assert!(validate_host_outputs(&outputs, Some((1280, 800))).is_err());
        outputs.remove(&2);
        for invalid in [
            OutputGeometry {
                scale: 2,
                ..normal.clone()
            },
            OutputGeometry {
                x: -1280,
                ..normal.clone()
            },
            OutputGeometry {
                normal: false,
                ..normal.clone()
            },
        ] {
            outputs.insert(1, invalid);
            assert!(validate_host_outputs(&outputs, Some((1280, 800))).is_err());
        }
    }
    #[test]
    fn input_validation() {
        let mut a = action("click");
        a.x = Some(1280);
        a.y = Some(0);
        assert!(input_commands(&a).is_err());
        a.x = Some(1279);
        assert!(input_commands(&a).is_ok());
        let mut a = action("key");
        a.key = Some("CTRL+a".into());
        assert_eq!(input_commands(&a).unwrap().len(), 4);
        a.key = Some("shell:whatever".into());
        assert!(input_commands(&a).is_err());
    }
    #[tokio::test]
    async fn status_does_not_start_or_stop_another_owner() {
        let controller = DesktopController::new(PathBuf::from("unused"));
        assert_eq!(controller.status().await["running"], false);
        assert!(controller.inner.session.lock().await.is_none());
        controller.stop(None).await.unwrap();
    }
    #[tokio::test]
    #[ignore = "Starts only a self-owned virtual KWin and fixed GTK fixture; requires KDE, Python GI Gtk/Atspi"]
    async fn real_private_desktop_smoke() {
        let controller = DesktopController::new(PathBuf::from("unused"));
        let result = async {
            let mut launch = action("launch");
            // Start isolated compositor before resolving its fixed embedded fixture path.
            let _ = controller
                .execute("smoke", &action("screenshot"), CancellationToken::new())
                .await?;
            let helper = controller
                .inner
                .session
                .lock()
                .await
                .as_ref()
                .unwrap()
                .root
                .join("native_accessibility.py");
            launch.argv = Some(vec![
                "/usr/bin/python3".into(),
                helper.to_string_lossy().into_owned(),
                "fixture".into(),
            ]);
            controller
                .execute("smoke", &launch, CancellationToken::new())
                .await?;
            let mut observed = Value::Null;
            for _ in 0..20 {
                let response = controller
                    .execute("smoke", &action("observe"), CancellationToken::new())
                    .await?;
                observed = serde_json::from_str(&response.text)?;
                if observed["nodes"]
                    .as_array()
                    .is_some_and(|n| n.iter().any(|n| n["name"] == "Fixture input"))
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            let nodes = observed["nodes"]
                .as_array()
                .context("Accessibility nodes")?;
            let input_node = nodes
                .iter()
                .find(|n| n["name"] == "Fixture input")
                .context("Fixture entry missing")?;
            let input = input_node["ref_id"].as_str().unwrap().to_owned();
            let rect = input_node["rect"]
                .as_array()
                .context("Fixture input rect missing")?;
            let center = (
                rect[0].as_i64().unwrap() + rect[2].as_i64().unwrap() / 2,
                rect[1].as_i64().unwrap() + rect[3].as_i64().unwrap() / 2,
            );
            let button = nodes
                .iter()
                .find(|n| n["name"] == "Fixture click")
                .context("Fixture button missing")?["ref_id"]
                .as_str()
                .unwrap()
                .to_owned();
            let mut fill = action("fill");
            fill.ref_id = Some(input);
            fill.text = Some("Private accessibility ✓".into());
            controller
                .execute("smoke", &fill, CancellationToken::new())
                .await?;
            let mut click = action("click");
            click.ref_id = Some(button);
            controller
                .execute("smoke", &click, CancellationToken::new())
                .await?;
            let screenshot = controller
                .execute("smoke", &action("screenshot"), CancellationToken::new())
                .await?;
            ensure!(screenshot.image.is_some(), "Screenshot missing");
            let mut coordinate = action("click");
            coordinate.x = Some(center.0 as i32);
            coordinate.y = Some(center.1 as i32);
            controller
                .execute("smoke", &coordinate, CancellationToken::new())
                .await?;
            let mut end = action("key");
            end.key = Some("End".into());
            controller
                .execute("smoke", &end, CancellationToken::new())
                .await?;
            let mut typing = action("type");
            typing.text = Some(" Typed Ω".into());
            controller
                .execute("smoke", &typing, CancellationToken::new())
                .await?;
            tokio::time::sleep(Duration::from_millis(150)).await;
            let response = controller
                .execute("smoke", &action("observe"), CancellationToken::new())
                .await?;
            ensure!(
                response.text.contains("Fixture clicked"),
                "AX button action did not update fixture"
            );
            ensure!(
                response.text.contains("Typed Ω"),
                "Private fake_input Unicode typing not observed: {}",
                response.text
            );
            ensure!(
                controller
                    .execute("smoke", &fill, CancellationToken::new())
                    .await
                    .is_err(),
                "Stale accessibility ref was accepted"
            );
            let mut wrong = action("observe");
            wrong.target = Some("desktop".into());
            ensure!(
                controller
                    .execute("other", &wrong, CancellationToken::new())
                    .await
                    .is_err(),
                "Lease hijack allowed"
            );
            Ok::<(), anyhow::Error>(())
        }
        .await;
        controller.stop(None).await.unwrap();
        result.unwrap();
    }

    #[tokio::test]
    #[ignore = "Tests real KWin FakeInput coordinate click on un-focused GTK fixture button"]
    async fn real_private_desktop_coordinate_click_smoke() {
        let controller = DesktopController::new(PathBuf::from("unused"));
        let result = async {
            let _ = controller
                .execute("smoke", &action("screenshot"), CancellationToken::new())
                .await?;
            let helper = controller
                .inner
                .session
                .lock()
                .await
                .as_ref()
                .unwrap()
                .root
                .join("native_accessibility.py");
            let mut launch = action("launch");
            launch.argv = Some(vec![
                "/usr/bin/python3".into(),
                helper.to_string_lossy().into_owned(),
                "fixture".into(),
            ]);
            controller
                .execute("smoke", &launch, CancellationToken::new())
                .await?;
            for _ in 0..20 {
                let response = controller
                    .execute("smoke", &action("observe"), CancellationToken::new())
                    .await?;
                if response.text.contains("Fixture click") {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            let ocr_out = controller
                .execute("smoke", &action("ocr"), CancellationToken::new())
                .await?;
            eprintln!("OCR: {}", ocr_out.text);
            let mut click_text = action("click_text");
            click_text.text = Some("Fixture click".into());
            let click_res = controller
                .execute("smoke", &click_text, CancellationToken::new())
                .await?;
            eprintln!("CLICK_TEXT: {}", click_res.text);
            ensure!(click_res.image.is_some(), "click_text should return annotated screenshot");
            tokio::time::sleep(Duration::from_millis(150)).await;
            let obs = controller
                .execute("smoke", &action("observe"), CancellationToken::new())
                .await?;
            ensure!(
                obs.text.contains("Fixture clicked"),
                "Coordinate click_text did NOT click button! observe={}",
                obs.text
            );
            Ok::<(), anyhow::Error>(())
        }
        .await;
        controller.stop(None).await.unwrap();
        result.unwrap();
    }
}
