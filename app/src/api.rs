//! The local API: what the window shows and does, over HTTP on 127.0.0.1, for
//! scripts and diagnostics.
//!
//! At start the app writes two files to its data folder: `api_port` (the port
//! number) and `api_token` (64 hex characters, kept across restarts). Every
//! request under `/api` needs the header `Authorization: Bearer <token>`, so a
//! web page in a browser cannot drive the app. `/openapi.json` (the OpenAPI
//! document) and `/docs` (a Swagger page for trying requests) need no token;
//! they hold no state.
//!
//! The handlers call `Backend` on a blocking thread: it uses a blocking HTTP
//! client, and the window holds the same lock for each frame. Node queries
//! (`/api/node/...`) are passed through to the embedded node's REST server as
//! it answers them.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path as UrlPath, Query, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use jcoin::amount::format_jef;
use jcoin_gpu_miner::kernel::Kernel;
use serde::{Deserialize, Serialize};
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{IntoParams, Modify, OpenApi, ToSchema};
use utoipa_swagger_ui::SwaggerUi;

use crate::applog::{self, AppLog};
use crate::backend::{self, Backend, MiningState};
use crate::install::Placement;
use crate::update::{self, UpdateState, Updates};

/// File in the data folder that holds the API's port.
pub const PORT_FILE: &str = "api_port";

/// File in the data folder that holds the API's token.
pub const TOKEN_FILE: &str = "api_token";

/// The port tried first; another free one is used if it is taken.
const PREFERRED_PORT: u16 = 3100;

/// Delay before an action that ends the process, so its reply is sent first.
const EXIT_DELAY: Duration = Duration::from_millis(500);

/// What the window and the API share.
#[derive(Clone)]
pub struct Shared {
    pub backend: Arc<Mutex<Backend>>,
    pub updates: Updates,
    pub place: Placement,
}

#[derive(Clone)]
struct AppState {
    shared: Shared,
    token: Arc<String>,
    http: reqwest::Client,
    node_url: String,
    data_dir: PathBuf,
    log: Arc<AppLog>,
}

/// Start the API on its own thread. Writes the port and token files and logs
/// the address; logs the reason if it cannot start. The window works either way.
pub fn start(shared: Shared) {
    let (data_dir, node_url, log) = {
        let b = shared.backend.lock().unwrap();
        (b.settings.data_dir.clone(), b.node_url().to_string(), b.log.clone())
    };
    let prepared = (|| -> Result<(TcpListener, String), String> {
        let token = load_or_create_token(&data_dir)?;
        let listener = TcpListener::bind(("127.0.0.1", PREFERRED_PORT))
            .or_else(|_| TcpListener::bind(("127.0.0.1", 0)))
            .map_err(|e| format!("cannot open a port: {e}"))?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        std::fs::write(data_dir.join(PORT_FILE), port.to_string())
            .map_err(|e| format!("cannot write {PORT_FILE}: {e}"))?;
        Ok((listener, token))
    })();
    let (listener, token) = match prepared {
        Ok(p) => p,
        Err(e) => {
            log.push(true, &format!("[app] local API not started: {e}"));
            return;
        }
    };
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
    let http = match reqwest::Client::builder().timeout(Duration::from_secs(10)).build() {
        Ok(h) => h,
        Err(e) => {
            log.push(true, &format!("[app] local API not started: {e}"));
            return;
        }
    };
    let state = AppState {
        shared,
        token: Arc::new(token),
        http,
        node_url,
        data_dir,
        log: log.clone(),
    };
    log.push(false, &format!("[app] local API on http://127.0.0.1:{port} (docs at /docs)"));
    std::thread::spawn(move || {
        let result = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|e| e.to_string())
            .and_then(|rt| {
                rt.block_on(async move {
                    let listener = tokio::net::TcpListener::from_std(listener).map_err(|e| e.to_string())?;
                    axum::serve(listener, router(state)).await.map_err(|e| e.to_string())
                })
            });
        if let Err(e) = result {
            log.push(true, &format!("[app] local API stopped: {e}"));
        }
    });
}

/// The token in `TOKEN_FILE`, or a new random one written there (readable by
/// this user only, on Unix).
fn load_or_create_token(dir: &Path) -> Result<String, String> {
    let path = dir.join(TOKEN_FILE);
    if let Ok(text) = std::fs::read_to_string(&path) {
        let text = text.trim();
        if text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Ok(text.to_string());
        }
    }
    let token = hex::encode(rand::random::<[u8; 32]>());
    write_private(&path, &token).map_err(|e| format!("cannot write {TOKEN_FILE}: {e}"))?;
    Ok(token)
}

fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        file.write_all(text.as_bytes())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, text)
    }
}

fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/status", get(status))
        .route("/node/chain", get(node_chain))
        .route("/node/chain/recent", get(node_recent))
        .route("/node/peers", get(node_peers))
        .route("/node/signal", get(node_signal))
        .route("/node/balance/{address}", get(node_balance))
        .route("/node/transaction/{id}", get(node_transaction))
        .route("/keys", get(list_keys).post(create_key))
        .route("/keys/import", post(import_key))
        .route("/keys/active", put(select_key))
        .route("/keys/active/secret", get(secret))
        .route("/send", post(send))
        .route("/sent", get(sent))
        .route("/mining", get(mining))
        .route("/mining/start", post(start_mining))
        .route("/mining/stop", post(stop_mining))
        .route("/mining/gpu-limit", put(set_gpu_limit))
        .route("/mining/kernel", put(set_kernel))
        .route("/mining/blocks", get(won_blocks))
        .route("/gpus", get(gpus))
        .route("/updates", get(updates))
        .route("/updates/check", post(check_updates))
        .route("/updates/install", post(install_update))
        .route("/install", get(install_state).post(install_app))
        .route("/uninstall", post(uninstall))
        .route("/restart", post(restart))
        .route("/device", get(device).put(set_device))
        .route("/diagnostics", get(diagnostics))
        .route("/log", get(log))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_token))
        .with_state(state);
    Router::new()
        .nest("/api", api)
        .merge(SwaggerUi::new("/docs").url("/openapi.json", ApiDoc::openapi()))
}

async fn require_token(State(s): State<AppState>, request: Request, next: Next) -> Response {
    let given = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    if !same(given.trim().as_bytes(), s.token.as_bytes()) {
        return ApiError(
            StatusCode::UNAUTHORIZED,
            format!("send the header 'Authorization: Bearer <token>', with the token from {TOKEN_FILE} in the data folder"),
        )
        .into_response();
    }
    next.run(request).await
}

/// Compare without stopping at the first difference.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

// --- errors ------------------------------------------------------------------

/// An error reply: `{"error": "..."}`.
#[derive(Serialize, ToSchema)]
struct ErrorReply {
    error: String,
}

struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(ErrorReply { error: self.1 })).into_response()
    }
}

fn bad(e: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, e.into())
}

fn conflict(e: impl Into<String>) -> ApiError {
    ApiError(StatusCode::CONFLICT, e.into())
}

type ApiResult<T> = Result<Json<T>, ApiError>;

/// A reply to an action that runs on after the reply.
#[derive(Serialize, ToSchema)]
struct MessageReply {
    message: String,
}

fn accepted(message: impl Into<String>) -> (StatusCode, Json<MessageReply>) {
    (StatusCode::ACCEPTED, Json(MessageReply { message: message.into() }))
}

/// Run `f` with the backend on a blocking thread.
async fn with_backend<T: Send + 'static>(
    s: &AppState,
    f: impl FnOnce(&mut Backend) -> T + Send + 'static,
) -> T {
    let backend = s.shared.backend.clone();
    tokio::task::spawn_blocking(move || f(&mut backend.lock().unwrap()))
        .await
        .expect("a local API handler panicked")
}

/// Run `f` after the reply is sent; log its error if it returns one. For
/// actions that end the process.
fn after_reply(log: Arc<AppLog>, what: &'static str, f: impl FnOnce() -> Result<(), String> + Send + 'static) {
    std::thread::spawn(move || {
        std::thread::sleep(EXIT_DELAY);
        if let Err(e) = f() {
            log.push(true, &format!("[app] local API: {what} failed: {e}"));
        }
    });
}

// --- status ------------------------------------------------------------------

/// The app at a glance: node, key, account, mining, updates.
#[derive(Serialize, ToSchema)]
struct StatusReply {
    /// This build's version.
    version: String,
    /// `mainnet` or `regtest`.
    network: String,
    /// The operating system (`linux`, `windows`, `macos`).
    os: String,
    data_dir: String,
    /// The label sent with reports and to peers; empty if none is set.
    device_label: String,
    /// The embedded node's own REST server.
    node_url: String,
    node: NodeStatus,
    /// The key in use.
    key: KeyReply,
    /// The key in use's account.
    account: Account,
    mining: MiningReply,
    newer_rules: NewerRules,
    /// The newest release, if it is newer than this build.
    newer_release: Option<String>,
    /// This program is the installed copy.
    installed_copy: bool,
}

#[derive(Serialize, ToSchema)]
struct NodeStatus {
    /// The node's REST server has answered at least once.
    online: bool,
    /// Why the node stopped, if it did.
    fatal: Option<String>,
    height: u64,
    /// Connections that exchange blocks (sessions).
    sessions: usize,
    /// Sessions carried by the relay.
    relayed_sessions: usize,
    /// All open connections, including the relay's.
    connections: usize,
    /// The relay holds a reservation: other nodes can reach this one.
    reservation: bool,
    /// The tip's difficulty target, as hex.
    target: String,
    /// Leading zero bits of the target.
    difficulty_bits: u32,
    /// Average hashes needed for the next block.
    next_expected_hashes: f64,
    /// The chain's target seconds between blocks.
    target_block_time: u64,
    /// The average seconds between the recent blocks.
    average_gap: Option<f64>,
}

#[derive(Serialize, ToSchema)]
struct Account {
    /// The figures below have been read for the key in use (false for a
    /// moment after a key switch).
    known: bool,
    /// Confirmed balance, base units.
    balance: u64,
    /// The same, as decimal JEF.
    balance_jef: String,
    /// Base units committed to pending transfers.
    pending: u64,
    pending_jef: String,
    /// Confirmed nonce: this key's transfers in the chain.
    nonce: u64,
}

/// Evidence that the network follows newer rules than this build knows.
#[derive(Serialize, ToSchema)]
struct NewerRules {
    /// `none`, `notice`, or `strong` (empty before the first reply).
    level: String,
    newest_version: u64,
    known_version: u64,
    blocks: u64,
    work_ratio: f64,
}

/// The app at a glance: node, key in use, account, mining, newer rules,
/// updates. One call answers "what state is this app in".
#[utoipa::path(get, path = "/api/status", tag = "app",
    responses((status = 200, body = StatusReply)))]
async fn status(State(s): State<AppState>) -> Json<StatusReply> {
    let device_label = crate::telemetry::device_name(&s.data_dir);
    let newer_release = s.shared.updates.newer_release();
    let installed_copy = s.shared.place.installed_copy;
    let reply = with_backend(&s, move |b| {
        let v = b.view();
        let sessions = v.connections.iter().filter(|c| c.session).count();
        StatusReply {
            version: update::CURRENT.to_string(),
            network: if b.settings.regtest { "regtest" } else { "mainnet" }.to_string(),
            os: std::env::consts::OS.to_string(),
            data_dir: b.settings.data_dir.display().to_string(),
            device_label,
            node_url: b.node_url().to_string(),
            node: NodeStatus {
                online: v.online,
                fatal: v.fatal.clone(),
                height: v.height,
                sessions,
                relayed_sessions: v.connections.iter().filter(|c| c.session && c.relayed).count(),
                connections: v.connections.len(),
                reservation: v.advertised.iter().any(|a| a.contains("p2p-circuit")),
                target: v.target.clone(),
                difficulty_bits: backend::zero_bits(&v.target),
                next_expected_hashes: v.next_expected_hashes,
                target_block_time: v.target_block_time,
                average_gap: v.average_gap,
            },
            key: KeyReply {
                name: b.key_name.clone(),
                address: b.address.clone(),
                active: true,
            },
            account: Account {
                known: v.account_known,
                balance: v.balance,
                balance_jef: format_jef(v.balance),
                pending: v.pending,
                pending_jef: format_jef(v.pending),
                nonce: v.nonce,
            },
            mining: mining_reply(b),
            newer_rules: NewerRules {
                level: v.signal.level.clone(),
                newest_version: v.signal.newest_version,
                known_version: v.signal.known_version,
                blocks: v.signal.blocks,
                work_ratio: v.signal.work_ratio,
            },
            newer_release,
            installed_copy,
        }
    })
    .await;
    Json(reply)
}

// --- node pass-through ---------------------------------------------------------

/// Forward a GET to the embedded node's REST server; its status and JSON body
/// come back unchanged.
async fn node_get(s: &AppState, path: &str) -> Response {
    let reply = match s.http.get(format!("{}{path}", s.node_url)).send().await {
        Ok(r) => r,
        Err(e) => return ApiError(StatusCode::BAD_GATEWAY, format!("the node did not answer: {e}")).into_response(),
    };
    let status = StatusCode::from_u16(reply.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    match reply.json::<serde_json::Value>().await {
        Ok(body) => (status, Json(body)).into_response(),
        Err(e) => ApiError(StatusCode::BAD_GATEWAY, format!("the node's reply is not JSON: {e}")).into_response(),
    }
}

/// The node's `GET /chain`: height, tip target, peers, expected work.
#[utoipa::path(get, path = "/api/node/chain", tag = "node",
    responses((status = 200, description = "The node's reply", body = serde_json::Value),
              (status = 502, body = ErrorReply)))]
async fn node_chain(State(s): State<AppState>) -> Response {
    node_get(&s, "/chain").await
}

#[derive(Deserialize, IntoParams)]
struct RecentQuery {
    /// How many blocks, newest first (the node caps it at 100).
    count: Option<u64>,
}

/// The node's `GET /chain/recent`: the last blocks and their average gap.
#[utoipa::path(get, path = "/api/node/chain/recent", tag = "node", params(RecentQuery),
    responses((status = 200, description = "The node's reply", body = serde_json::Value),
              (status = 502, body = ErrorReply)))]
async fn node_recent(State(s): State<AppState>, Query(q): Query<RecentQuery>) -> Response {
    let count = q.count.unwrap_or(backend::RECENT_BLOCKS);
    node_get(&s, &format!("/chain/recent?count={count}")).await
}

/// The node's `GET /peers`: its own addresses and every open connection.
#[utoipa::path(get, path = "/api/node/peers", tag = "node",
    responses((status = 200, description = "The node's reply", body = serde_json::Value),
              (status = 502, body = ErrorReply)))]
async fn node_peers(State(s): State<AppState>) -> Response {
    node_get(&s, "/peers").await
}

/// The node's `GET /network/signal`: blocks seen with newer rules.
#[utoipa::path(get, path = "/api/node/signal", tag = "node",
    responses((status = 200, description = "The node's reply", body = serde_json::Value),
              (status = 502, body = ErrorReply)))]
async fn node_signal(State(s): State<AppState>) -> Response {
    node_get(&s, "/network/signal").await
}

/// The node's `GET /balance/{address}`: balance, pending, nonce of any address.
#[utoipa::path(get, path = "/api/node/balance/{address}", tag = "node",
    params(("address" = String, Path, description = "64 hex characters")),
    responses((status = 200, description = "The node's reply", body = serde_json::Value),
              (status = 502, body = ErrorReply)))]
async fn node_balance(State(s): State<AppState>, UrlPath(address): UrlPath<String>) -> Response {
    if !is_hex(&address, 64) {
        return bad("the address must be 64 hex characters").into_response();
    }
    node_get(&s, &format!("/balance/{address}")).await
}

/// The node's `GET /wallet/transaction/{id}`: whether a transfer is pending,
/// confirmed, or unknown.
#[utoipa::path(get, path = "/api/node/transaction/{id}", tag = "node",
    params(("id" = String, Path, description = "Transaction id, 64 hex characters")),
    responses((status = 200, description = "The node's reply", body = serde_json::Value),
              (status = 502, body = ErrorReply)))]
async fn node_transaction(State(s): State<AppState>, UrlPath(id): UrlPath<String>) -> Response {
    if !is_hex(&id, 64) {
        return bad("the id must be 64 hex characters").into_response();
    }
    node_get(&s, &format!("/wallet/transaction/{id}")).await
}

fn is_hex(text: &str, len: usize) -> bool {
    text.len() == len && text.bytes().all(|b| b.is_ascii_hexdigit())
}

// --- keys --------------------------------------------------------------------

/// A stored key.
#[derive(Serialize, ToSchema)]
struct KeyReply {
    name: String,
    /// Its address, 64 hex characters.
    address: String,
    /// It is the key in use: it receives mining rewards and signs sends.
    active: bool,
}

#[derive(Deserialize, ToSchema)]
struct KeyName {
    name: String,
}

#[derive(Deserialize, ToSchema)]
struct ImportKey {
    name: String,
    /// The secret key, 64 hex characters.
    secret: String,
}

#[derive(Serialize, ToSchema)]
struct SecretReply {
    name: String,
    address: String,
    /// The secret key, 64 hex characters. Whoever has it can spend from the
    /// address.
    secret: String,
}

fn key_reply(b: &Backend, name: &str) -> Option<KeyReply> {
    b.keys().iter().find(|k| k.name == name).map(|k| KeyReply {
        name: k.name.clone(),
        address: k.address.clone(),
        active: k.name == b.key_name,
    })
}

/// The stored keys, sorted by name.
#[utoipa::path(get, path = "/api/keys", tag = "keys",
    responses((status = 200, body = Vec<KeyReply>)))]
async fn list_keys(State(s): State<AppState>) -> Json<Vec<KeyReply>> {
    Json(
        with_backend(&s, |b| {
            let names: Vec<String> = b.keys().iter().map(|k| k.name.clone()).collect();
            names.iter().filter_map(|n| key_reply(b, n)).collect()
        })
        .await,
    )
}

/// Create a new key. It is stored but not put in use.
#[utoipa::path(post, path = "/api/keys", tag = "keys", request_body = KeyName,
    responses((status = 201, body = KeyReply), (status = 400, body = ErrorReply)))]
async fn create_key(State(s): State<AppState>, Json(req): Json<KeyName>) -> Result<(StatusCode, Json<KeyReply>), ApiError> {
    with_backend(&s, move |b| {
        b.create_key(&req.name)?;
        key_reply(b, req.name.trim()).ok_or_else(|| "the key was created but cannot be read".to_string())
    })
    .await
    .map(|k| (StatusCode::CREATED, Json(k)))
    .map_err(bad)
}

/// Store an existing key from its secret. It is stored but not put in use.
#[utoipa::path(post, path = "/api/keys/import", tag = "keys", request_body = ImportKey,
    responses((status = 201, body = KeyReply), (status = 400, body = ErrorReply)))]
async fn import_key(State(s): State<AppState>, Json(req): Json<ImportKey>) -> Result<(StatusCode, Json<KeyReply>), ApiError> {
    with_backend(&s, move |b| {
        b.import_key(&req.name, req.secret.trim())?;
        key_reply(b, req.name.trim()).ok_or_else(|| "the key was imported but cannot be read".to_string())
    })
    .await
    .map(|k| (StatusCode::CREATED, Json(k)))
    .map_err(bad)
}

/// Put a key in use. Refused while mining: the running miner pays the address
/// it was started with.
#[utoipa::path(put, path = "/api/keys/active", tag = "keys", request_body = KeyName,
    responses((status = 200, body = KeyReply), (status = 400, body = ErrorReply),
              (status = 409, description = "Mining is running", body = ErrorReply)))]
async fn select_key(State(s): State<AppState>, Json(req): Json<KeyName>) -> ApiResult<KeyReply> {
    with_backend(&s, move |b| {
        if b.mining_adapter().is_some() {
            return Err(conflict("stop mining before switching keys"));
        }
        b.select_key(&req.name).map_err(bad)?;
        key_reply(b, &req.name).ok_or_else(|| bad("no such key"))
    })
    .await
    .map(Json)
}

/// The secret key of the key in use, for a backup.
#[utoipa::path(get, path = "/api/keys/active/secret", tag = "keys",
    responses((status = 200, body = SecretReply)))]
async fn secret(State(s): State<AppState>) -> Json<SecretReply> {
    Json(
        with_backend(&s, |b| SecretReply {
            name: b.key_name.clone(),
            address: b.address.clone(),
            secret: b.secret_hex(),
        })
        .await,
    )
}

// --- sending -----------------------------------------------------------------

#[derive(Deserialize, ToSchema)]
struct SendRequest {
    /// The recipient's address, 64 hex characters.
    recipient: String,
    /// Decimal JEF, for example `"1.5"`.
    amount: String,
    /// Decimal JEF; empty or missing means no fee.
    #[serde(default)]
    fee: Option<String>,
}

#[derive(Serialize, ToSchema)]
struct SendReply {
    /// The transaction id, 64 hex characters.
    id: String,
}

/// A transfer sent from this app.
#[derive(Serialize, ToSchema)]
struct SentReply {
    id: String,
    sender: String,
    recipient: String,
    /// Base units.
    amount: u64,
    amount_jef: String,
    nonce: u64,
    /// In the chain: the sender's confirmed nonce has passed this one.
    confirmed: bool,
}

/// Sign a transfer with the key in use and submit it to the node.
#[utoipa::path(post, path = "/api/send", tag = "wallet", request_body = SendRequest,
    responses((status = 200, body = SendReply),
              (status = 400, description = "Bad input, too little balance, or refused by the node", body = ErrorReply)))]
async fn send(State(s): State<AppState>, Json(req): Json<SendRequest>) -> ApiResult<SendReply> {
    with_backend(&s, move |b| b.send(&req.recipient, &req.amount, req.fee.as_deref().unwrap_or("")))
        .await
        .map(|id| Json(SendReply { id }))
        .map_err(bad)
}

/// Transfers sent from the key in use, newest first, as the window lists them.
#[utoipa::path(get, path = "/api/sent", tag = "wallet",
    responses((status = 200, body = Vec<SentReply>)))]
async fn sent(State(s): State<AppState>) -> Json<Vec<SentReply>> {
    Json(
        with_backend(&s, |b| {
            let confirmed_nonce = b.view().nonce;
            b.sent
                .iter()
                .rev()
                .filter(|tx| tx.sender == b.address)
                .map(|tx| SentReply {
                    id: tx.id.clone(),
                    sender: tx.sender.clone(),
                    recipient: tx.recipient.clone(),
                    amount: tx.amount,
                    amount_jef: format_jef(tx.amount),
                    nonce: tx.nonce,
                    confirmed: confirmed_nonce > tx.nonce,
                })
                .collect()
        })
        .await,
    )
}

// --- mining ------------------------------------------------------------------

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
enum MiningStateName {
    Idle,
    /// The GPU is being opened.
    Starting,
    Running,
    /// Mining stopped by itself; see `error`.
    Failed,
}

#[derive(Serialize, ToSchema)]
struct MiningReply {
    state: MiningStateName,
    /// Why mining failed, if it did.
    error: Option<String>,
    /// The GPU index mining was started on; null for the system's choice or
    /// when not mining.
    adapter: Option<usize>,
    /// The address that receives rewards (the key in use).
    rewards_to: String,
    /// GPU use limit, percent.
    gpu_limit: u32,
    /// The GPU kernel in use.
    kernel: String,
    /// The kernels that can be chosen.
    kernels: Vec<String>,
    /// Hashes per second; 0 when not running.
    hash_rate: u64,
    /// The height being worked on.
    height: u64,
    /// Blocks accepted in this mining run.
    blocks_found: u64,
    /// The miner's last event.
    status: String,
    /// Average seconds to a block for this computer alone at the current
    /// difficulty.
    expected_seconds: Option<f64>,
}

fn mining_reply(b: &mut Backend) -> MiningReply {
    let (state, error) = match b.mining_state() {
        MiningState::Idle => (MiningStateName::Idle, None),
        MiningState::Starting => (MiningStateName::Starting, None),
        MiningState::Running => (MiningStateName::Running, None),
        MiningState::Failed(e) => (MiningStateName::Failed, Some(e)),
    };
    let (hash_rate, height, blocks_found, status) = b.mining_stats().unwrap_or_default();
    MiningReply {
        state,
        error,
        adapter: b.mining_adapter().flatten(),
        rewards_to: b.address.clone(),
        gpu_limit: b.gpu_limit(),
        kernel: b.kernel().name().to_string(),
        kernels: Kernel::ALL.iter().map(|k| k.name().to_string()).collect(),
        hash_rate,
        height,
        blocks_found,
        status,
        expected_seconds: backend::expected_seconds(b.view().next_expected_hashes, hash_rate),
    }
}

#[derive(Deserialize, ToSchema)]
struct StartMining {
    /// GPU index from `GET /api/gpus`; null or missing for the system's choice.
    #[serde(default)]
    adapter: Option<usize>,
}

#[derive(Deserialize, ToSchema)]
struct GpuLimit {
    /// Percent of full power, 10 to 100 (clamped).
    percent: u32,
}

#[derive(Deserialize, ToSchema)]
struct KernelChoice {
    /// One of the names in `kernels` of `GET /api/mining`.
    kernel: String,
}

/// What a block this app mined looks like.
#[derive(Serialize, ToSchema)]
struct WonBlock {
    height: u64,
    hash: String,
    /// Unix seconds.
    timestamp: u64,
    /// Subsidy plus fees, base units.
    reward: u64,
    reward_jef: String,
    /// Transfers, not counting the coinbase.
    transfers: u64,
    target: String,
}

#[derive(Serialize, ToSchema)]
struct GpuReply {
    /// The `adapter` value for `POST /api/mining/start`.
    index: usize,
    name: String,
}

#[derive(Serialize, ToSchema)]
struct GpusReply {
    /// False while the GPUs are still being listed, just after start.
    listed: bool,
    gpus: Vec<GpuReply>,
}

/// The miner's state and settings.
#[utoipa::path(get, path = "/api/mining", tag = "mining",
    responses((status = 200, body = MiningReply)))]
async fn mining(State(s): State<AppState>) -> Json<MiningReply> {
    Json(with_backend(&s, mining_reply).await)
}

/// Start mining with rewards to the key in use. Send `{}` for the system's
/// choice of GPU.
#[utoipa::path(post, path = "/api/mining/start", tag = "mining", request_body = StartMining,
    responses((status = 200, body = MiningReply), (status = 400, body = ErrorReply),
              (status = 409, description = "Already mining", body = ErrorReply)))]
async fn start_mining(State(s): State<AppState>, Json(req): Json<StartMining>) -> ApiResult<MiningReply> {
    with_backend(&s, move |b| {
        if b.mining_adapter().is_some() {
            return Err(conflict("already mining"));
        }
        if let (Some(i), Some(gpus)) = (req.adapter, b.gpus()) {
            if i >= gpus.len() {
                return Err(bad(format!("no GPU {i}; there are {}", gpus.len())));
            }
        }
        b.start_mining(req.adapter);
        Ok(mining_reply(b))
    })
    .await
    .map(Json)
}

/// Stop mining. The miner ends within one GPU dispatch.
#[utoipa::path(post, path = "/api/mining/stop", tag = "mining",
    responses((status = 200, body = MiningReply)))]
async fn stop_mining(State(s): State<AppState>) -> Json<MiningReply> {
    Json(
        with_backend(&s, |b| {
            b.stop_mining();
            mining_reply(b)
        })
        .await,
    )
}

/// Set the GPU use limit. It applies at once and is remembered.
#[utoipa::path(put, path = "/api/mining/gpu-limit", tag = "mining", request_body = GpuLimit,
    responses((status = 200, body = MiningReply)))]
async fn set_gpu_limit(State(s): State<AppState>, Json(req): Json<GpuLimit>) -> Json<MiningReply> {
    Json(
        with_backend(&s, move |b| {
            b.set_gpu_limit(req.percent);
            mining_reply(b)
        })
        .await,
    )
}

/// Choose the GPU kernel. It is remembered; if mining, mining restarts with it.
#[utoipa::path(put, path = "/api/mining/kernel", tag = "mining", request_body = KernelChoice,
    responses((status = 200, body = MiningReply), (status = 400, body = ErrorReply)))]
async fn set_kernel(State(s): State<AppState>, Json(req): Json<KernelChoice>) -> ApiResult<MiningReply> {
    let kernel = Kernel::parse(req.kernel.trim()).map_err(bad)?;
    Ok(Json(
        with_backend(&s, move |b| {
            b.set_kernel(kernel);
            mining_reply(b)
        })
        .await,
    ))
}

/// Blocks mined since the app started, oldest first.
#[utoipa::path(get, path = "/api/mining/blocks", tag = "mining",
    responses((status = 200, body = Vec<WonBlock>)))]
async fn won_blocks(State(s): State<AppState>) -> Json<Vec<WonBlock>> {
    let won = with_backend(&s, |b| b.won_blocks()).await;
    Json(
        won.into_iter()
            .map(|f| WonBlock {
                height: f.height,
                hash: f.hash,
                timestamp: f.timestamp,
                reward: f.reward,
                reward_jef: format_jef(f.reward),
                transfers: f.transfers,
                target: f.target,
            })
            .collect(),
    )
}

/// The GPUs mining can use.
#[utoipa::path(get, path = "/api/gpus", tag = "mining",
    responses((status = 200, body = GpusReply)))]
async fn gpus(State(s): State<AppState>) -> Json<GpusReply> {
    let names = with_backend(&s, |b| b.gpus()).await;
    Json(GpusReply {
        listed: names.is_some(),
        gpus: names
            .unwrap_or_default()
            .into_iter()
            .enumerate()
            .map(|(index, name)| GpuReply { index, name })
            .collect(),
    })
}

// --- updates and install -------------------------------------------------------

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
enum UpdateStateName {
    /// The release list is being read.
    Checking,
    /// The release list was read; see `releases`.
    Listed,
    /// A release is being downloaded; the app restarts into it.
    Installing,
    /// The check or the install failed; see `error`.
    Failed,
}

#[derive(Serialize, ToSchema)]
struct ReleaseReply {
    /// For example `v2.4.0`.
    tag: String,
    /// `YYYY-MM-DD`.
    date: String,
    /// `newer`, `this version`, or `older`.
    relation: String,
}

#[derive(Serialize, ToSchema)]
struct UpdatesReply {
    /// This build's version.
    current: String,
    state: UpdateStateName,
    error: Option<String>,
    /// The tag being installed.
    installing: Option<String>,
    /// Releases with a build for this system, newest first.
    releases: Vec<ReleaseReply>,
    /// The newest release, if it is newer than this build.
    newer_release: Option<String>,
}

#[derive(Deserialize, ToSchema)]
struct InstallUpdate {
    /// A tag from `releases`, with or without the `v`.
    tag: String,
    /// Stop mining first if it is running. Without it, an install is refused
    /// while mining.
    #[serde(default)]
    stop_mining: bool,
}

fn updates_reply(u: &Updates) -> UpdatesReply {
    let (state, error, installing, releases) = match u.state() {
        UpdateState::Checking => (UpdateStateName::Checking, None, None, Vec::new()),
        UpdateState::Listed(list) => (UpdateStateName::Listed, None, None, list),
        UpdateState::Installing(tag) => (UpdateStateName::Installing, None, Some(tag), Vec::new()),
        UpdateState::Failed(e) => (UpdateStateName::Failed, Some(e), None, Vec::new()),
    };
    UpdatesReply {
        current: update::CURRENT.to_string(),
        state,
        error,
        installing,
        releases: releases
            .iter()
            .map(|r| ReleaseReply {
                tag: r.tag.clone(),
                date: r.date.clone(),
                relation: r.relation().to_string(),
            })
            .collect(),
        newer_release: u.newer_release(),
    }
}

/// The release list and any install in progress. The app reads the list at
/// start and every 30 minutes.
#[utoipa::path(get, path = "/api/updates", tag = "updates",
    responses((status = 200, body = UpdatesReply)))]
async fn updates(State(s): State<AppState>) -> Json<UpdatesReply> {
    Json(updates_reply(&s.shared.updates))
}

/// Read the release list again. Poll `GET /api/updates` for the result.
#[utoipa::path(post, path = "/api/updates/check", tag = "updates",
    responses((status = 200, body = UpdatesReply),
              (status = 409, description = "An install is in progress", body = ErrorReply)))]
async fn check_updates(State(s): State<AppState>) -> ApiResult<UpdatesReply> {
    if matches!(s.shared.updates.state(), UpdateState::Installing(_)) {
        return Err(conflict("an install is in progress"));
    }
    s.shared.updates.check();
    Ok(Json(updates_reply(&s.shared.updates)))
}

/// Download a release, check it, replace this program, and restart into it.
/// Refused for the same reasons the window greys out its button. Mining does
/// not resume after the restart.
#[utoipa::path(post, path = "/api/updates/install", tag = "updates", request_body = InstallUpdate,
    responses((status = 202, body = MessageReply), (status = 400, body = ErrorReply),
              (status = 409, description = "Refused now: see the error", body = ErrorReply)))]
async fn install_update(
    State(s): State<AppState>,
    Json(req): Json<InstallUpdate>,
) -> Result<(StatusCode, Json<MessageReply>), ApiError> {
    let list = match s.shared.updates.state() {
        UpdateState::Listed(list) => list,
        UpdateState::Installing(tag) => return Err(conflict(format!("{tag} is being installed"))),
        _ => return Err(conflict("the release list has not been read; POST /api/updates/check first")),
    };
    let want = format!("v{}", req.tag.trim().trim_start_matches('v'));
    let release = list
        .into_iter()
        .find(|r| r.tag == want)
        .ok_or_else(|| bad(format!("{want} is not in the release list for this system")))?;
    let not_installed = s.shared.place.offer;
    let stop = req.stop_mining;
    let refusal = with_backend(&s, {
        let release = release.clone();
        move |b| {
            let mining = b.mining_adapter().is_some();
            match update::install_refusal(&release, not_installed, mining && !stop) {
                Some(reason) => Some(reason),
                None => {
                    if mining {
                        b.stop_mining();
                    }
                    None
                }
            }
        }
    })
    .await;
    if let Some(reason) = refusal {
        return Err(conflict(reason));
    }
    let tag = release.tag.clone();
    s.shared.updates.install(release);
    Ok(accepted(format!("installing {tag}; the app restarts when it is ready")))
}

#[derive(Serialize, ToSchema)]
struct InstallReply {
    /// This system supports installing.
    supported: bool,
    /// This program is the installed copy.
    installed_copy: bool,
    /// The window offers to install this copy.
    offer: bool,
    /// The installed version, if any copy is installed.
    installed_version: Option<String>,
    /// Where the installed program goes.
    installed_program: Option<String>,
    /// This program file.
    running_program: String,
}

/// Where this copy stands relative to the installed one.
#[utoipa::path(get, path = "/api/install", tag = "updates",
    responses((status = 200, body = InstallReply)))]
async fn install_state(State(s): State<AppState>) -> Json<InstallReply> {
    let p = &s.shared.place;
    Json(InstallReply {
        supported: p.layout.is_some(),
        installed_copy: p.installed_copy,
        offer: p.offer,
        installed_version: p.layout.as_ref().and_then(|l| l.installed_version()),
        installed_program: p.layout.as_ref().map(|l| l.program.display().to_string()),
        running_program: p.exe.display().to_string(),
    })
}

/// Install this copy (replacing an installed one), then start the installed
/// copy and end this one. Only when the window offers it.
#[utoipa::path(post, path = "/api/install", tag = "updates",
    responses((status = 202, body = MessageReply), (status = 400, body = ErrorReply),
              (status = 409, description = "Not offered for this copy", body = ErrorReply)))]
async fn install_app(State(s): State<AppState>) -> Result<(StatusCode, Json<MessageReply>), ApiError> {
    let place = s.shared.place.clone();
    let Some(layout) = place.layout.clone().filter(|_| place.offer) else {
        return Err(conflict(if place.installed_copy {
            "this is the installed copy"
        } else {
            "installing is not offered here (unsupported system or test settings)"
        }));
    };
    let exe = place.exe.clone();
    tokio::task::spawn_blocking(move || layout.install(&exe, update::CURRENT))
        .await
        .expect("install task panicked")
        .map_err(bad)?;
    let program = place.layout.as_ref().map(|l| l.program.clone()).unwrap_or_default();
    after_reply(s.log.clone(), "starting the installed copy", move || {
        crate::install::hand_over_to(&program)
    });
    Ok(accepted("installed; starting the installed copy and ending this one"))
}

/// Remove the installed app and end it. The data folder (keys and chain)
/// stays. Only for the installed copy.
#[utoipa::path(post, path = "/api/uninstall", tag = "updates",
    responses((status = 202, body = MessageReply),
              (status = 409, description = "Not the installed copy", body = ErrorReply)))]
async fn uninstall(State(s): State<AppState>) -> Result<(StatusCode, Json<MessageReply>), ApiError> {
    let place = s.shared.place.clone();
    if !place.installed_copy {
        return Err(conflict("this is not the installed copy"));
    }
    after_reply(s.log.clone(), "uninstall", move || place.uninstall_and_exit());
    Ok(accepted("removing the app; it ends now and the data folder stays"))
}

/// Start this program again and end this process. Mining does not resume.
#[utoipa::path(post, path = "/api/restart", tag = "app",
    responses((status = 202, body = MessageReply)))]
async fn restart(State(s): State<AppState>) -> (StatusCode, Json<MessageReply>) {
    let exe = s.shared.place.exe.clone();
    after_reply(s.log.clone(), "restart", move || update::restart(&exe));
    accepted("restarting")
}

// --- device label, diagnostics, log ---------------------------------------------

#[derive(Serialize, Deserialize, ToSchema)]
struct Device {
    /// The label for this computer, sent with reports and to peers. Empty
    /// removes it.
    label: String,
}

/// The label for this computer.
#[utoipa::path(get, path = "/api/device", tag = "app",
    responses((status = 200, body = Device)))]
async fn device(State(s): State<AppState>) -> Json<Device> {
    Json(Device {
        label: crate::telemetry::device_name(&s.data_dir),
    })
}

/// Set the label for this computer. It also answers the first-run prompt.
/// Peers see a new label after the next restart.
#[utoipa::path(put, path = "/api/device", tag = "app", request_body = Device,
    responses((status = 200, body = Device), (status = 400, body = ErrorReply)))]
async fn set_device(State(s): State<AppState>, Json(req): Json<Device>) -> ApiResult<Device> {
    let saved = crate::telemetry::set_device_name(&s.data_dir, &req.label).map_err(bad)?;
    let _ = std::fs::write(s.data_dir.join(crate::DEVICE_PROMPTED_FILE), "");
    s.log.push(false, &format!("[app] device label set to '{saved}'"));
    Ok(Json(Device { label: saved }))
}

/// The text report the window's "Copy diagnostics" button copies: versions,
/// node state, connections, blocks mined, and the end of the log. No secrets.
#[utoipa::path(get, path = "/api/diagnostics", tag = "app",
    responses((status = 200, body = String, content_type = "text/plain")))]
async fn diagnostics(State(s): State<AppState>) -> String {
    with_backend(&s, |b| b.diagnostics()).await
}

#[derive(Deserialize, IntoParams)]
struct LogQuery {
    /// The last N lines (default 200).
    lines: Option<usize>,
    /// Only error lines.
    #[serde(default)]
    errors: bool,
}

#[derive(Serialize, ToSchema)]
struct LogLineReply {
    /// Unix seconds.
    time: u64,
    /// The same, as UTC text.
    utc: String,
    error: bool,
    text: String,
}

/// The end of the app's log, oldest first: the lines the window's log shows.
#[utoipa::path(get, path = "/api/log", tag = "app", params(LogQuery),
    responses((status = 200, body = Vec<LogLineReply>)))]
async fn log(State(s): State<AppState>, Query(q): Query<LogQuery>) -> Json<Vec<LogLineReply>> {
    let tail: Vec<_> = s.log.tail().into_iter().filter(|l| !q.errors || l.error).collect();
    let skip = tail.len().saturating_sub(q.lines.unwrap_or(200));
    Json(
        tail.into_iter()
            .skip(skip)
            .map(|l| LogLineReply {
                time: l.time,
                utc: applog::utc(l.time),
                error: l.error,
                text: l.text,
            })
            .collect(),
    )
}

// --- the OpenAPI document ------------------------------------------------------

#[derive(OpenApi)]
#[openapi(
    info(
        title = "JeffyCoin app API",
        description = "The desktop app's local API: what the window shows and does. It listens on 127.0.0.1 only. \
                       The port is in the file `api_port` and the token in `api_token`, both in the app's data folder. \
                       Send `Authorization: Bearer <token>` with every request under /api (use Authorize on this page). \
                       Amounts are in base units with a decimal JEF copy beside them; requests take decimal JEF."
    ),
    paths(
        status, restart, device, set_device, diagnostics, log,
        node_chain, node_recent, node_peers, node_signal, node_balance, node_transaction,
        list_keys, create_key, import_key, select_key, secret,
        send, sent,
        mining, start_mining, stop_mining, set_gpu_limit, set_kernel, won_blocks, gpus,
        updates, check_updates, install_update, install_state, install_app, uninstall,
    ),
    modifiers(&TokenScheme),
    security(("token" = [])),
    tags(
        (name = "app", description = "State, restart, label, diagnostics, log"),
        (name = "node", description = "The embedded node's REST replies, passed through"),
        (name = "keys", description = "Stored keys and the key in use"),
        (name = "wallet", description = "Sending and the sent list"),
        (name = "mining", description = "The GPU miner"),
        (name = "updates", description = "Releases, updating, installing"),
    )
)]
pub struct ApiDoc;

struct TokenScheme;

impl Modify for TokenScheme {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "token",
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .description(Some("The contents of `api_token` in the data folder"))
                    .build(),
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::Settings;
    use serde_json::{json, Value};
    use std::time::Instant;

    struct Client {
        base: String,
        token: String,
        http: reqwest::blocking::Client,
    }

    impl Client {
        /// `(status, JSON body or null)`.
        fn call(&self, method: &str, path: &str, body: Option<Value>, token: Option<&str>) -> (u16, Value) {
            let url = format!("{}{path}", self.base);
            let mut req = match method {
                "GET" => self.http.get(url),
                "POST" => self.http.post(url),
                "PUT" => self.http.put(url),
                other => panic!("method {other}"),
            };
            if let Some(t) = token {
                req = req.bearer_auth(t);
            }
            if let Some(b) = body {
                req = req.json(&b);
            }
            let reply = req.send().unwrap();
            let status = reply.status().as_u16();
            (status, reply.json().unwrap_or(Value::Null))
        }
        fn get(&self, path: &str) -> (u16, Value) {
            self.call("GET", path, None, Some(&self.token))
        }
        fn post(&self, path: &str, body: Value) -> (u16, Value) {
            self.call("POST", path, Some(body), Some(&self.token))
        }
        fn put(&self, path: &str, body: Value) -> (u16, Value) {
            self.call("PUT", path, Some(body), Some(&self.token))
        }
    }

    fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
        let start = Instant::now();
        while !done() {
            assert!(start.elapsed() < Duration::from_secs(30), "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    /// The API on a backend on a private regtest chain: the token is required,
    /// every documented GET answers, and keys, mining settings, sending, the
    /// label, updates and install behave as the window's buttons do. Needs no
    /// GPU and does nothing that ends the process.
    #[test]
    fn the_api_does_what_the_window_does() {
        let dir = std::env::temp_dir().join(format!("jcoin-app-api-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let backend = Backend::start(Settings {
            data_dir: dir.clone(),
            relay: None,
            peers: Vec::new(),
            regtest: true,
        })
        .unwrap();
        let shared = Shared {
            backend: Arc::new(Mutex::new(backend)),
            updates: Updates::with_state(UpdateState::Failed("not checked in tests".into())),
            place: Placement {
                layout: None,
                exe: std::env::current_exe().unwrap(),
                installed_copy: false,
                offer: false,
            },
        };
        start(shared.clone());
        let port: u16 = std::fs::read_to_string(dir.join(PORT_FILE)).unwrap().trim().parse().unwrap();
        let token = std::fs::read_to_string(dir.join(TOKEN_FILE)).unwrap();
        assert!(is_hex(&token, 64));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join(TOKEN_FILE)).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "the token is readable by this user only");
        }
        let c = Client {
            base: format!("http://127.0.0.1:{port}"),
            token: token.clone(),
            http: reqwest::blocking::Client::new(),
        };

        // The token is required; the document and the docs page are not.
        assert_eq!(c.call("GET", "/api/status", None, None).0, 401);
        assert_eq!(c.call("GET", "/api/status", None, Some(&"0".repeat(64))).0, 401);
        let (code, doc) = c.call("GET", "/openapi.json", None, None);
        assert_eq!(code, 200);
        let docs = c.http.get(format!("{}/docs/", c.base)).send().unwrap();
        assert_eq!(docs.status().as_u16(), 200);
        assert!(docs.text().unwrap().contains("swagger"));

        // Every documented GET without a path parameter answers.
        wait_for("the node", || c.get("/api/status").1["node"]["online"] == true);
        let paths = doc["paths"].as_object().unwrap();
        assert!(paths.len() >= 25, "{} paths documented", paths.len());
        for (path, ops) in paths {
            assert!(ops.as_object().unwrap().values().all(|op| op["responses"].is_object()));
            if ops.get("get").is_some() && !path.contains('{') {
                let (code, body) = c.get(path);
                // The diagnostics are text, not JSON.
                assert_eq!(code, 200, "GET {path}: {body}");
            }
        }

        let status = c.get("/api/status").1;
        assert_eq!(status["network"], "regtest");
        assert_eq!(status["version"], update::CURRENT);
        assert_eq!(status["key"]["name"], "wallet");
        let first = status["key"]["address"].as_str().unwrap().to_string();
        let (code, bal) = c.get(&format!("/api/node/balance/{first}"));
        assert_eq!((code, bal["balance"].as_u64()), (200, Some(0)));
        assert_eq!(c.get("/api/node/balance/xyz").0, 400);
        assert_eq!(c.get("/api/node/chain").1["height"], 0);

        // Keys: create, refuse a duplicate, select, read the secret, import it.
        let (code, key) = c.post("/api/keys", json!({"name": "savings"}));
        assert_eq!((code, key["active"].as_bool()), (201, Some(false)));
        assert_eq!(c.post("/api/keys", json!({"name": "savings"})).0, 400);
        let (code, key) = c.put("/api/keys/active", json!({"name": "savings"}));
        assert_eq!((code, key["active"].as_bool()), (200, Some(true)));
        assert_ne!(key["address"], first);
        assert_eq!(c.put("/api/keys/active", json!({"name": "nope"})).0, 400);
        let secret = c.get("/api/keys/active/secret").1;
        assert_eq!(secret["address"], key["address"]);
        let (code, copy) = c.post("/api/keys/import", json!({"name": "copy", "secret": secret["secret"]}));
        assert_eq!((code, &copy["address"]), (201, &key["address"]));
        assert_eq!(c.get("/api/keys").1.as_array().unwrap().len(), 3);
        assert_eq!(c.get("/api/status").1["key"]["name"], "savings");

        // Mining settings, without starting the GPU.
        let m = c.put("/api/mining/gpu-limit", json!({"percent": 3})).1;
        assert_eq!(m["gpu_limit"], backend::MIN_GPU_LIMIT);
        assert_eq!(m["state"], "idle");
        assert_eq!(c.put("/api/mining/kernel", json!({"kernel": "warp-drive"})).0, 400);
        let m = c.put("/api/mining/kernel", json!({"kernel": "precomputed"})).1;
        assert_eq!(m["kernel"], "precomputed");
        // A GPU index past the list is refused once the GPUs are listed.
        wait_for("the GPU list", || c.get("/api/gpus").1["listed"] == true);
        assert_eq!(c.post("/api/mining/start", json!({"adapter": 999})).0, 400);
        assert_eq!(c.post("/api/mining/stop", json!({})).1["state"], "idle");

        // Sending with no coins is refused with the reason.
        let (code, err) = c.post("/api/send", json!({"recipient": "11".repeat(32), "amount": "1"}));
        assert_eq!(code, 400);
        assert!(err["error"].as_str().unwrap().contains("balance too low"), "{err}");
        assert_eq!(c.get("/api/sent").1, json!([]));

        // The label, the log, and the diagnostics.
        assert_eq!(c.put("/api/device", json!({"label": "Test box"})).1["label"], "Test box");
        assert_eq!(c.get("/api/device").1["label"], "Test box");
        assert!(dir.join(crate::DEVICE_PROMPTED_FILE).exists());
        let log = c.get("/api/log?lines=5000").1;
        assert!(log.as_array().unwrap().iter().any(|l| l["text"].as_str().unwrap().starts_with("[app] local API on")));
        let report = c.http.get(format!("{}/api/diagnostics", c.base)).bearer_auth(&token).send().unwrap().text().unwrap();
        assert!(report.contains("network: regtest"));
        assert!(!report.contains(secret["secret"].as_str().unwrap()), "no secret in the report");

        // Updates and install refuse what the window would not offer.
        assert_eq!(c.get("/api/updates").1["state"], "failed");
        assert_eq!(c.post("/api/updates/install", json!({"tag": "v99.0.0"})).0, 409);
        assert_eq!(c.post("/api/install", json!({})).0, 409);
        assert_eq!(c.post("/api/uninstall", json!({})).0, 409);

        // The token survives a restart of the API.
        assert_eq!(load_or_create_token(&dir).unwrap(), token);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
