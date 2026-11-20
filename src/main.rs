#![deny(unsafe_code)]
#[cfg(not(target_os = "linux"))]
compile_error!("JustAIAgent supports Linux only.");

mod agent;
mod autonomy_store;
mod cache_audit;
mod computer;
mod computer_browser;
mod computer_desktop;
mod computer_fake_input;
mod computer_host;
mod files;
mod image_tool;
mod message_queue;
mod models;
mod network;
mod privilege;
mod quality;
mod quality_network;
mod quality_store;
mod quality_transport;
mod session_crypto;
mod store;
mod subagent_models;
mod tools;
mod web_tools;

use anyhow::{Context, Result, bail, ensure};
use argon2::{
    Argon2, PasswordHash, PasswordHasher, PasswordVerifier,
    password_hash::{SaltString, rand_core::OsRng},
};
use axum::{
    Json, Router,
    body::Body,
    extract::{ConnectInfo, DefaultBodyLimit, Path, Query, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{
        IntoResponse, Response, Sse,
        sse::{Event as SseEvent, KeepAlive},
    },
    routing::{get, patch, post},
};
use models::*;
use rand::RngCore;
use rust_embed::RustEmbed;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    convert::Infallible,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use store::Store;
use tokio::sync::{Mutex, Semaphore};

#[derive(RustEmbed)]
#[folder = "web/dist/"]
struct Assets;

#[derive(Clone)]
struct AppState {
    store: Store,
    engine: agent::Engine,
    quality: quality::QualityChecks,
    options: Arc<Options>,
    attempts: Arc<Mutex<HashMap<IpAddr, Vec<Instant>>>>,
    password_slots: Arc<Semaphore>,
    shutdown: tokio_util::sync::CancellationToken,
}
#[derive(Debug)]
struct Options {
    bind: IpAddr,
    port: u16,
    data_dir: PathBuf,
    workspace: PathBuf,
    public_url: Option<url::Url>,
    hosts: HashSet<String>,
    origins: HashSet<String>,
}

impl Options {
    fn parse() -> Result<Self> {
        let cwd = std::env::current_dir()?;
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| cwd.clone());
        let mut bind = "127.0.0.1".to_owned();
        let mut port = 4141u16;
        let mut data_dir = std::env::var_os("JUSTAI_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share/justaiagent"));
        let mut workspace = cwd;
        let mut public = std::env::var("JUSTAI_PUBLIC_URL").ok();
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--bind" => bind = args.next().context("--bind needs an IP address")?,
                "--port" => port = args.next().context("--port needs a port")?.parse()?,
                "--data-dir" => {
                    data_dir = PathBuf::from(args.next().context("--data-dir needs a path")?)
                }
                "--workspace" => {
                    workspace = PathBuf::from(args.next().context("--workspace needs a path")?)
                }
                "--public-url" => {
                    public = Some(args.next().context("--public-url needs an HTTPS URL")?)
                }
                "--help" | "-h" => {
                    println!(
                        "JustAIAgent — Linux local AI, controlled from a browser\n\njustaiagent [--bind 127.0.0.1] [--port 4141] [--workspace PATH]\n            [--data-dir PATH] [--public-url https://agent.example.com]\n\nSet the initial password locally before exposing the application.\nRemote access requires an HTTPS reverse proxy/tunnel and --public-url.\nNo interactive CLI, plugins or MCP."
                    );
                    std::process::exit(0);
                }
                _ => bail!("Unknown launch option: {arg}"),
            }
        }
        let bind: IpAddr = bind.parse().context("Bind must be a literal IP address")?;
        ensure!(port > 0, "Port must be nonzero");
        let public_url = public
            .map(|p| url::Url::parse(&p))
            .transpose()
            .context("Invalid public URL")?;
        if let Some(u) = &public_url {
            ensure!(
                u.scheme() == "https"
                    && u.host_str().is_some()
                    && u.username().is_empty()
                    && u.password().is_none()
                    && u.path() == "/"
                    && u.query().is_none()
                    && u.fragment().is_none(),
                "Public URL must be a credential-free HTTPS origin without a path"
            );
        }
        ensure!(
            bind.is_loopback() || public_url.is_some(),
            "Non-loopback bind requires --public-url and an HTTPS reverse proxy"
        );
        std::fs::create_dir_all(&data_dir)?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&data_dir, std::fs::Permissions::from_mode(0o700))?;
        let data_dir = std::fs::canonicalize(data_dir)?;
        let workspace = std::fs::canonicalize(workspace).context("Workspace does not exist")?;
        ensure!(workspace.is_dir(), "Workspace must be a directory");
        let mut hosts = HashSet::new();
        let mut origins = HashSet::new();
        for host in [
            "127.0.0.1".to_owned(),
            "localhost".to_owned(),
            "[::1]".to_owned(),
        ] {
            hosts.insert(format!("{host}:{port}"));
            origins.insert(format!("http://{host}:{port}"));
        }
        if let Some(u) = &public_url {
            hosts.insert(u[url::Position::BeforeHost..url::Position::AfterPort].to_lowercase());
            origins.insert(u.origin().ascii_serialization());
        }
        Ok(Self {
            bind,
            port,
            data_dir,
            workspace,
            public_url,
            hosts,
            origins,
        })
    }
    fn is_local_host(&self, headers: &HeaderMap) -> bool {
        request_host(headers).is_some_and(|h| {
            h == format!("127.0.0.1:{}", self.port)
                || h == format!("localhost:{}", self.port)
                || h == format!("[::1]:{}", self.port)
        })
    }
    fn secure_cookie(&self, headers: &HeaderMap) -> bool {
        !self.is_local_host(headers) && self.public_url.is_some()
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "justaiagent=info,tower_http=warn".into()),
        )
        .init();
    let options = Options::parse()?;
    use std::os::unix::fs::OpenOptionsExt;
    let lock_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(options.data_dir.join("instance.lock"))?;
    let _instance_lock =
        nix::fcntl::Flock::lock(lock_file, nix::fcntl::FlockArg::LockExclusiveNonblock).map_err(
            |_| anyhow::anyhow!("Another JustAIAgent instance owns this data directory"),
        )?;
    ensure!(
        Assets::get("index.html").is_some(),
        "Web assets missing. Run npm ci && npm run build in web before cargo build."
    );
    let store = Store::open(&options.data_dir.join("justai.sqlite3"))?;
    ensure!(
        options.public_url.is_none() || store.password_hash().await?.is_some(),
        "Initialize the password locally without --public-url before enabling remote access"
    );
    let engine = agent::Engine::new(store.clone(), options.data_dir.clone());
    let address = SocketAddr::new(options.bind, options.port);
    let state = AppState {
        quality: quality::QualityChecks::new(store.clone()),
        store,
        engine,
        options: Arc::new(options),
        attempts: Arc::new(Mutex::new(HashMap::new())),
        password_slots: Arc::new(Semaphore::new(2)),
        shutdown: tokio_util::sync::CancellationToken::new(),
    };
    let router = app(state.clone());
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .context("Cannot bind application listener")?;
    tracing::info!("JustAIAgent ready at http://{address}");
    if let Some(url) = &state.options.public_url {
        tracing::info!(
            "Configured remote origin: {}",
            url.origin().ascii_serialization()
        );
    }
    if state.store.password_hash().await?.is_none() {
        tracing::info!(
            "Set the initial password at http://127.0.0.1:{}/ before exposing the service",
            address.port()
        );
    }
    let shutdown_state = state.clone();
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        shutdown_signal().await;
        shutdown_state.shutdown.cancel();
        if let Ok(sessions) = shutdown_state.store.sessions().await {
            for session in sessions {
                let _ = shutdown_state.engine.cancel(&session.id).await;
            }
        }
    })
    .await?;
    Ok(())
}
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        }
    };
    tokio::select! {_ =ctrl_c=>{},_=terminate=>{}}
}
fn app(state: AppState) -> Router {
    Router::new()
        .route("/api/bootstrap", get(bootstrap))
        .route("/api/setup", post(setup))
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/password", post(change_password))
        .route("/api/config", get(get_config).put(put_config))
        .route("/api/network/test", post(test_network))
        .route("/api/computer", get(computer_status))
        .route("/api/computer/stop", post(computer_stop))
        .route("/api/computer/host", post(computer_host_policy))
        .route(
            "/api/sessions/{id}/computer/image/{capture_id}",
            get(computer_capture),
        )
        .route("/api/sessions", get(list_sessions).post(create_session))
        .route("/api/sessions/{id}", get(session_detail).delete(delete_session_route))
        .route(
            "/api/sessions/{id}/encryption",
            post(set_session_encryption_route),
        )
        .route("/api/sessions/{id}/unlock", post(unlock_session_route))
        .route("/api/sessions/{id}/lock", post(lock_session_route))
        .route(
            "/api/sessions/{id}/settings",
            patch(update_session_settings),
        )
        .route("/api/sessions/{id}/files", get(session_files))
        .route("/api/sessions/{id}/file", get(preview_file))
        .route("/api/sessions/{id}/export", get(export_session))
        .route("/api/sessions/{id}/questions", get(session_questions))
        .route(
            "/api/sessions/{id}/questions/{question_id}/answer",
            post(answer_question),
        )
        .route(
            "/api/sessions/{id}/goals",
            get(session_goals).post(create_goal),
        )
        .route("/api/sessions/{id}/goals/{goal_id}", patch(change_goal))
        .route("/api/model-checks/plan", post(quality_plan))
        .route("/api/model-checks", get(quality_list).post(quality_start))
        .route("/api/model-checks/{id}", get(quality_detail))
        .route("/api/model-checks/{id}/cancel", post(quality_cancel))
        .route("/api/model-checks/{id}/export", get(quality_export))
        .route("/api/sessions/{id}/messages", post(send_message))
        .route(
            "/api/sessions/{id}/messages/interrupt",
            post(interrupt_message),
        )
        .route("/api/sessions/{id}/cancel", post(cancel_session))
        .route("/api/sessions/{id}/compact", post(compact_session))
        .route("/api/sessions/{id}/events", get(session_events))
        .fallback(static_asset)
        .layer(DefaultBodyLimit::max(24 * 1024 * 1024))
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state)
}

#[derive(Debug)]
struct ApiError(StatusCode, String);
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error":self.1}))).into_response()
    }
}
impl From<anyhow::Error> for ApiError {
    fn from(error: anyhow::Error) -> Self {
        let text = error.to_string();
        let status = if text.contains("CONFIG_CONFLICT")
            || text.contains("SESSION_BUSY")
            || text.contains("already running")
            || text.contains("already active")
        {
            StatusCode::CONFLICT
        } else if text == "Session not found" {
            StatusCode::NOT_FOUND
        } else {
            StatusCode::BAD_REQUEST
        };
        Self(status, text)
    }
}
type ApiResult<T> = std::result::Result<T, ApiError>;
fn request_host(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_ascii_lowercase())
}
fn token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|s| s.trim().strip_prefix("justai_session="))
        .filter(|t| t.len() == 64 && t.bytes().all(|b| b.is_ascii_hexdigit()))
}
fn token_hash(t: &str) -> String {
    format!("{:x}", Sha256::digest(t.as_bytes()))
}
async fn authenticated(state: &AppState, headers: &HeaderMap) -> bool {
    match token(headers) {
        Some(t) => state
            .store
            .auth_session(token_hash(t))
            .await
            .unwrap_or(false),
        None => false,
    }
}

async fn guard(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let path = request.uri().path().to_owned();
    if state.shutdown.is_cancelled() {
        return ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "Server is shutting down".into(),
        )
        .into_response();
    }
    if request_host(request.headers()).is_none_or(|h| !state.options.hosts.contains(&h)) {
        return ApiError(
            StatusCode::FORBIDDEN,
            "Unrecognized Host. Configure the exact public HTTPS origin.".into(),
        )
        .into_response();
    }
    if request.method() != axum::http::Method::GET && request.method() != axum::http::Method::HEAD {
        if request
            .headers()
            .get("x-justai-request")
            .and_then(|v| v.to_str().ok())
            != Some("1")
        {
            return ApiError(
                StatusCode::FORBIDDEN,
                "Missing same-origin request marker".into(),
            )
            .into_response();
        }
        if let Some(origin) = request.headers().get(header::ORIGIN) {
            if origin
                .to_str()
                .ok()
                .is_none_or(|o| !state.options.origins.contains(o))
            {
                return ApiError(StatusCode::FORBIDDEN, "Origin not allowed".into())
                    .into_response();
            }
        } else if request
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .is_none_or(|p| !p.0.ip().is_loopback())
            || !state.options.is_local_host(request.headers())
        {
            return ApiError(
                StatusCode::FORBIDDEN,
                "Remote mutations require an allowed Origin".into(),
            )
            .into_response();
        }
    }
    let public = matches!(
        path.as_str(),
        "/api/bootstrap" | "/api/setup" | "/api/login"
    );
    if path.starts_with("/api/") && !public && !authenticated(&state, request.headers()).await {
        return ApiError(StatusCode::UNAUTHORIZED, "Please sign in".into()).into_response();
    }
    if !path.starts_with("/api/")
        && path != "/"
        && !path.starts_with("/assets/")
        && !path.starts_with("/brand/")
        && path != "/favicon.svg"
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    headers.insert("x-frame-options", HeaderValue::from_static("DENY"));
    if !headers.contains_key("content-security-policy") {
        headers.insert("content-security-policy",HeaderValue::from_static("default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; connect-src 'self'; font-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'"));
    }
    if path.starts_with("/api/") {
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    response
}

async fn bootstrap(
    State(state): State<AppState>,
    headers: HeaderMap,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let initialized = state.store.password_hash().await?.is_some();
    let auth = authenticated(&state, &headers).await;
    Ok(Json(
        json!({"initialized":initialized,"authenticated":auth,"allow_setup":peer.ip().is_loopback()&&state.options.is_local_host(&headers),"version":env!("CARGO_PKG_VERSION"),"workspace":if auth{Some(state.options.workspace.display().to_string())}else{None}}),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PasswordRequest {
    password: String,
}
fn validate_password(password: &str) -> ApiResult<()> {
    if !(12..=1024).contains(&password.len()) {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "Use a password of 12–1024 bytes".into(),
        ));
    }
    Ok(())
}
async fn hash_password(password: String) -> ApiResult<String> {
    tokio::task::spawn_blocking(move || {
        Argon2::default()
            .hash_password(password.as_bytes(), &SaltString::generate(&mut OsRng))
            .map(|h| h.to_string())
            .map_err(|_| {
                ApiError(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Password hashing failed".into(),
                )
            })
    })
    .await
    .map_err(|_| {
        ApiError(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Password worker unavailable".into(),
        )
    })?
}
async fn issue_session(
    state: &AppState,
    headers: &HeaderMap,
    expected_password_hash: String,
) -> ApiResult<Response> {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let token = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
    if !state
        .store
        .create_auth_session_checked(
            token_hash(&token),
            chrono::Utc::now().timestamp() + 7 * 24 * 3600,
            expected_password_hash,
        )
        .await?
    {
        return Err(ApiError(
            StatusCode::UNAUTHORIZED,
            "Password changed during sign-in; sign in again".into(),
        ));
    }
    let mut response = Json(json!({"ok":true})).into_response();
    let cookie = format!(
        "justai_session={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age=604800{}",
        if state.options.secure_cookie(headers) {
            "; Secure"
        } else {
            ""
        }
    );
    response
        .headers_mut()
        .insert(header::SET_COOKIE, HeaderValue::from_str(&cookie).unwrap());
    Ok(response)
}
async fn setup(
    State(state): State<AppState>,
    headers: HeaderMap,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(body): Json<PasswordRequest>,
) -> ApiResult<Response> {
    if !peer.ip().is_loopback() || !state.options.is_local_host(&headers) {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "Initial password must be set locally on the host computer".into(),
        ));
    }
    validate_password(&body.password)?;
    let _permit = state
        .password_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| {
            ApiError(
                StatusCode::TOO_MANY_REQUESTS,
                "Please wait before trying again".into(),
            )
        })?;
    if state.store.password_hash().await?.is_some() {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "Application is already initialized".into(),
        ));
    }
    let hash = hash_password(body.password).await?;
    if !state.store.initialize_password(hash.clone()).await? {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "Application was initialized by another request".into(),
        ));
    }
    issue_session(&state, &headers, hash).await
}
async fn check_login_rate(state: &AppState, ip: IpAddr) -> ApiResult<()> {
    let mut attempts = state.attempts.lock().await;
    let now = Instant::now();
    let cutoff = Duration::from_secs(15 * 60);
    attempts.retain(|_, v| {
        v.retain(|t| now.duration_since(*t) < cutoff);
        !v.is_empty()
    });
    if attempts.len() >= 4096 && !attempts.contains_key(&ip) {
        return Err(ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many sign-in attempts; try again later".into(),
        ));
    }
    let total: usize = attempts.values().map(Vec::len).sum();
    let list = attempts.entry(ip).or_default();
    if list.len() >= 12 || total >= 100 {
        return Err(ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many sign-in attempts; try again later".into(),
        ));
    }
    list.push(now);
    Ok(())
}
async fn verify_password(password: String, hash: String) -> ApiResult<bool> {
    tokio::task::spawn_blocking(move || {
        PasswordHash::new(&hash).ok().is_some_and(|hash| {
            Argon2::default()
                .verify_password(password.as_bytes(), &hash)
                .is_ok()
        })
    })
    .await
    .map_err(|_| {
        ApiError(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Password worker unavailable".into(),
        )
    })
}
async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(body): Json<PasswordRequest>,
) -> ApiResult<Response> {
    if body.password.len() > 1024 {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "Password is too long".into(),
        ));
    }
    check_login_rate(&state, peer.ip()).await?;
    let _permit = state
        .password_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| {
            ApiError(
                StatusCode::TOO_MANY_REQUESTS,
                "Please wait before trying again".into(),
            )
        })?;
    let hash = state.store.password_hash().await?.ok_or_else(|| {
        ApiError(
            StatusCode::FORBIDDEN,
            "Set the initial password locally first".into(),
        )
    })?;
    if !verify_password(body.password, hash.clone()).await? {
        return Err(ApiError(
            StatusCode::UNAUTHORIZED,
            "Incorrect password".into(),
        ));
    }
    issue_session(&state, &headers, hash).await
}
async fn logout(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Response> {
    if let Some(token) = token(&headers) {
        state.store.delete_auth_session(token_hash(token)).await?;
    }
    let mut response = Json(json!({"ok":true})).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&format!(
            "justai_session=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0{}",
            if state.options.secure_cookie(&headers) {
                "; Secure"
            } else {
                ""
            }
        ))
        .unwrap(),
    );
    Ok(response)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChangePassword {
    current_password: String,
    new_password: String,
}
async fn change_password(
    State(state): State<AppState>,
    headers: HeaderMap,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(body): Json<ChangePassword>,
) -> ApiResult<Response> {
    validate_password(&body.new_password)?;
    check_login_rate(&state, peer.ip()).await?;
    let _permit = state
        .password_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| {
            ApiError(
                StatusCode::TOO_MANY_REQUESTS,
                "Please wait before trying again".into(),
            )
        })?;
    let hash = state
        .store
        .password_hash()
        .await?
        .ok_or_else(|| ApiError(StatusCode::UNAUTHORIZED, "Please sign in".into()))?;
    if body.current_password.len() > 1024 || !verify_password(body.current_password, hash).await? {
        return Err(ApiError(
            StatusCode::UNAUTHORIZED,
            "Incorrect password".into(),
        ));
    }
    state
        .store
        .set_password_hash(hash_password(body.new_password).await?)
        .await?;
    state.store.revoke_auth_sessions().await?;
    logout(State(state), headers).await
}
async fn get_config(State(state): State<AppState>) -> ApiResult<Json<AppConfig>> {
    Ok(Json(state.store.config().await?.sanitized()))
}
async fn put_config(
    State(state): State<AppState>,
    Json(config): Json<AppConfig>,
) -> ApiResult<Json<AppConfig>> {
    Ok(Json(state.store.save_config(config).await?.sanitized()))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TestRequest {
    selection: Selection,
}
async fn computer_status(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    Ok(Json(state.engine.computer().status().await))
}
async fn computer_stop(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    state.engine.emergency_computer_stop().await?;
    Ok(Json(json!({"ok":true})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HostPolicy {
    enabled: bool,
}
async fn computer_host_policy(
    State(state): State<AppState>,
    Json(policy): Json<HostPolicy>,
) -> ApiResult<Json<Value>> {
    Ok(Json(
        state
            .engine
            .computer()
            .set_host_enabled(policy.enabled)
            .await?,
    ))
}
async fn computer_capture(
    State(state): State<AppState>,
    Path((id, capture_id)): Path<(String, String)>,
) -> ApiResult<Response> {
    state.store.session(&id).await?;
    let bytes = state.engine.computer().capture(&id, &capture_id).await?;
    let mut response = Response::new(Body::from(bytes));
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("image/png"));
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

async fn test_network(
    State(state): State<AppState>,
    Json(body): Json<TestRequest>,
) -> ApiResult<Json<Value>> {
    Ok(Json(state.engine.test_connection(body.selection).await?))
}
async fn list_sessions(State(state): State<AppState>) -> ApiResult<Json<Vec<Session>>> {
    Ok(Json(state.store.sessions().await?))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewSession {
    title: Option<String>,
    cwd: Option<String>,
    selection: Selection,
}
async fn create_session(
    State(state): State<AppState>,
    Json(body): Json<NewSession>,
) -> ApiResult<Json<Session>> {
    let config = state.store.config().await?;
    let site = config
        .sites
        .iter()
        .find(|s| s.id == body.selection.site_id)
        .ok_or_else(|| ApiError(StatusCode::BAD_REQUEST, "Select a configured site".into()))?;
    let key = site
        .keys
        .iter()
        .find(|k| k.id == body.selection.key_id)
        .ok_or_else(|| ApiError(StatusCode::BAD_REQUEST, "Select a configured key".into()))?;
    if !key.models.iter().any(|m| m.id == body.selection.model_id) {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "Select a configured model".into(),
        ));
    }
    let path = body
        .cwd
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| state.options.workspace.clone());
    let cwd = tokio::fs::canonicalize(path).await.map_err(|_| {
        ApiError(
            StatusCode::BAD_REQUEST,
            "Working directory does not exist".into(),
        )
    })?;
    if !cwd.is_dir() {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "Working directory must be a directory".into(),
        ));
    }
    let title = body.title.unwrap_or_else(|| "New session".into());
    if title.len() > 256 {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "Session title is too long".into(),
        ));
    }
    Ok(Json(
        state
            .store
            .create_session(title, cwd.display().to_string(), body.selection, None, 0)
            .await?,
    ))
}
async fn session_detail(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let session = state.store.session(&id).await?;
    let events = if session.locked {
        Vec::new()
    } else {
        state.store.events(&id, 0).await?
    };
    Ok(Json(json!({"session":session,"events":events})))
}
async fn delete_session_route(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    state.store.delete_session(&id).await?;
    Ok(Json(json!({"deleted":true})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionEncryptionInput {
    mode: String,
    #[serde(default)]
    password: Option<String>,
}
async fn set_session_encryption_route(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<SessionEncryptionInput>,
) -> ApiResult<Json<Session>> {
    Ok(Json(
        state
            .store
            .set_session_encryption(&id, &body.mode, body.password)
            .await?,
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionUnlockInput {
    password: String,
}
async fn unlock_session_route(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<SessionUnlockInput>,
) -> ApiResult<Json<Session>> {
    Ok(Json(state.store.unlock_session(&id, &body.password).await?))
}
async fn lock_session_route(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<Session>> {
    Ok(Json(state.store.lock_session(&id).await?))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionSettings {
    selection: Selection,
    #[serde(default)]
    reasoning_effort: Option<String>,
}
async fn update_session_settings(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<SessionSettings>,
) -> ApiResult<Json<Session>> {
    Ok(Json(
        state
            .store
            .update_session_settings(&id, body.selection, body.reasoning_effort)
            .await?,
    ))
}
#[derive(Deserialize, Default)]
struct FileQuery {
    #[serde(default)]
    path: String,
}
async fn session_files(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<FileQuery>,
) -> ApiResult<Json<Value>> {
    let session = state.store.session(&id).await?;
    Ok(Json(
        state
            .engine
            .files()
            .list(&id, &session.cwd, &query.path)
            .await?,
    ))
}
async fn preview_file(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<FileQuery>,
) -> ApiResult<Response> {
    let session = state.store.session(&id).await?;
    let (bytes, mime) = state
        .engine
        .files()
        .preview(&session.cwd, &query.path)
        .await?;
    let mut response = Response::new(Body::from(bytes));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&mime)
            .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "Unsupported preview MIME".into()))?,
    );
    // Even when opened in a top-level tab, HTML/SVG gets an opaque origin and
    // cannot execute code, submit forms, access cookies or call control APIs.
    response.headers_mut().insert("content-security-policy",HeaderValue::from_static("sandbox; default-src 'none'; script-src 'none'; connect-src 'none'; img-src data: blob:; style-src 'unsafe-inline'; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"));
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static(if mime == "application/octet-stream" {
            "attachment"
        } else {
            "inline"
        }),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        "cross-origin-resource-policy",
        HeaderValue::from_static("same-origin"),
    );
    Ok(response)
}
async fn export_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    let text = state.store.session_jsonl(&id).await?;
    let mut response = Response::new(Body::from(text));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/x-ndjson; charset=utf-8"),
    );
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename=\"{id}.jsonl\""))
            .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "Invalid session ID".into()))?,
    );
    Ok(response)
}

async fn session_questions(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<Question>>> {
    Ok(Json(state.store.questions(&id).await?))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AnswerRequest {
    answer: String,
}
async fn answer_question(
    State(state): State<AppState>,
    Path((id, question_id)): Path<(String, String)>,
    Json(body): Json<AnswerRequest>,
) -> ApiResult<Json<Question>> {
    Ok(Json(
        state
            .engine
            .answer_question(&id, &question_id, body.answer)
            .await?,
    ))
}
async fn session_goals(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<Goal>>> {
    Ok(Json(state.store.goals(&id).await?))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewGoal {
    title: String,
    objective: String,
}
async fn create_goal(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<NewGoal>,
) -> ApiResult<Json<Goal>> {
    Ok(Json(
        state
            .store
            .create_goal(&id, body.title, body.objective)
            .await?,
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GoalAction {
    action: String,
    title: Option<String>,
    objective: Option<String>,
}
async fn change_goal(
    State(state): State<AppState>,
    Path((id, goal_id)): Path<(String, String)>,
    Json(body): Json<GoalAction>,
) -> ApiResult<Json<Goal>> {
    Ok(Json(
        state
            .engine
            .change_goal(&id, &goal_id, &body.action, body.title, body.objective)
            .await?,
    ))
}

async fn quality_plan(
    State(state): State<AppState>,
    Json(input): Json<quality::CheckInput>,
) -> ApiResult<Json<Value>> {
    Ok(Json(state.quality.plan(input).await?))
}
async fn quality_start(
    State(state): State<AppState>,
    Json(input): Json<quality::CheckInput>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    Ok((
        StatusCode::ACCEPTED,
        Json(state.quality.start(input).await?),
    ))
}
async fn quality_list(
    State(state): State<AppState>,
    Query(selection): Query<Selection>,
) -> ApiResult<Json<Vec<Value>>> {
    Ok(Json(state.store.quality_runs(selection).await?))
}
async fn quality_detail(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    Ok(Json(state.store.quality_run(&id).await?))
}
async fn quality_cancel(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    Ok(Json(state.quality.cancel(&id).await?))
}
async fn quality_export(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    let run = state.store.quality_run(&id).await?;
    let body = serde_json::to_vec_pretty(&run).map_err(anyhow::Error::from)?;
    let mut response = Response::new(Body::from(body));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("attachment; filename=\"model-quality-check.json\""),
    );
    Ok(response)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InterruptMessage {
    client_message_id: String,
}
async fn interrupt_message(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<InterruptMessage>,
) -> ApiResult<Json<Value>> {
    Ok(Json(
        state
            .engine
            .interrupt_message(&id, &body.client_message_id)
            .await?,
    ))
}
async fn send_message(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<MessageInput>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({"run_id":state.engine.submit(&id,input).await?})),
    ))
}
async fn cancel_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    state.store.session(&id).await?;
    state.engine.cancel(&id).await?;
    Ok(Json(json!({"ok":true})))
}
async fn compact_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({"run_id":state.engine.compact(&id).await?})),
    ))
}
#[derive(Deserialize)]
struct EventQuery {
    #[serde(default)]
    after: i64,
}
async fn session_events(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<EventQuery>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    state.store.session(&id).await?;
    let auth_hash = token(&headers)
        .map(token_hash)
        .ok_or_else(|| ApiError(StatusCode::UNAUTHORIZED, "Please sign in".into()))?;
    let after = headers
        .get("last-event-id")
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(query.after)
        .max(0);
    let (tx, rx) = tokio::sync::mpsc::channel::<std::result::Result<SseEvent, Infallible>>(32);
    tokio::spawn(async move {
        let mut cursor = after;
        let mut interval = tokio::time::interval(Duration::from_millis(180));
        loop {
            tokio::select! {_=state.shutdown.cancelled()=>break,_=tx.closed()=>break,_=interval.tick()=>{}}
            if !state
                .store
                .auth_session(auth_hash.clone())
                .await
                .unwrap_or(false)
            {
                break;
            }
            match state.store.events(&id, cursor).await {
                Ok(events) => {
                    for event in events {
                        cursor = event.seq;
                        let encoded = match serde_json::to_string(&event) {
                            Ok(s) => s,
                            Err(_) => return,
                        };
                        let e = SseEvent::default()
                            .event("update")
                            .id(event.seq.to_string())
                            .data(encoded);
                        if !matches!(
                            tokio::time::timeout(Duration::from_secs(5), tx.send(Ok(e))).await,
                            Ok(Ok(()))
                        ) {
                            return;
                        }
                    }
                }
                Err(_) => break,
            }
        }
    });
    let mut response = Sse::new(tokio_stream::wrappers::ReceiverStream::new(rx))
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("heartbeat"),
        )
        .into_response();
    response
        .headers_mut()
        .insert("x-accel-buffering", HeaderValue::from_static("no"));
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache, no-transform"),
    );
    Ok(response)
}
async fn static_asset(request: Request) -> Response {
    if request.method() != axum::http::Method::GET && request.method() != axum::http::Method::HEAD {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let path = request.uri().path().trim_start_matches('/');
    let name = if path.is_empty() { "index.html" } else { path };
    match Assets::get(name) {
        Some(asset) => {
            let mime = mime_guess::from_path(name)
                .first_or_octet_stream()
                .to_string();
            let mut response = Response::new(Body::from(asset.data.into_owned()));
            response
                .headers_mut()
                .insert(header::CONTENT_TYPE, HeaderValue::from_str(&mime).unwrap());
            response.headers_mut().insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static(if name == "index.html" {
                    "no-store"
                } else {
                    "public, max-age=31536000, immutable"
                }),
            );
            response
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower::ServiceExt;
    fn test_state() -> (AppState, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("db")).unwrap();
        let mut hosts = HashSet::new();
        hosts.insert("127.0.0.1:4141".into());
        let mut origins = HashSet::new();
        origins.insert("http://127.0.0.1:4141".into());
        let options = Options {
            bind: "127.0.0.1".parse().unwrap(),
            port: 4141,
            data_dir: dir.path().into(),
            workspace: dir.path().into(),
            public_url: None,
            hosts,
            origins,
        };
        let state = AppState {
            engine: agent::Engine::new(store.clone(), dir.path().into()),
            quality: quality::QualityChecks::new(store.clone()),
            store,
            options: Arc::new(options),
            attempts: Arc::new(Mutex::new(HashMap::new())),
            password_slots: Arc::new(Semaphore::new(2)),
            shutdown: tokio_util::sync::CancellationToken::new(),
        };
        (state, dir)
    }
    #[tokio::test]
    async fn rejects_wrong_host_and_unauthenticated_api() {
        let (state, _d) = test_state();
        let response = app(state.clone())
            .oneshot(
                Request::builder()
                    .uri("/api/config")
                    .header("host", "evil.example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let response = app(state)
            .oneshot(
                Request::builder()
                    .uri("/api/config")
                    .header("host", "127.0.0.1:4141")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    #[tokio::test]
    async fn rejects_cross_origin_mutation() {
        let (state, _d) = test_state();
        let response = app(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/login")
                    .header("host", "127.0.0.1:4141")
                    .header("origin", "https://evil.example")
                    .header("x-justai-request", "1")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"password":"not-the-password"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
    #[tokio::test]
    async fn remote_setup_is_never_allowed() {
        let (state, _d) = test_state();
        let mut req = Request::builder()
            .method("POST")
            .uri("/api/setup")
            .header("host", "127.0.0.1:4141")
            .header("origin", "http://127.0.0.1:4141")
            .header("x-justai-request", "1")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"password":"a-long-password"}"#))
            .unwrap();
        req.extensions_mut().insert(ConnectInfo(
            "192.168.1.8:30000".parse::<SocketAddr>().unwrap(),
        ));
        assert_eq!(
            app(state).oneshot(req).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
    }
}
