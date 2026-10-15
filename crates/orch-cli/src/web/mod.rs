//! `orch web` — the TUI in a browser.
//!
//! Every websocket connection gets its own pseudo-terminal running `orch tui`
//! ([`pty`]), bridged to an xterm.js page. Binary frames carry raw terminal
//! bytes both ways; text frames carry JSON control messages
//! ([`parse_resize`]). Extra tabs are not shared sessions: the TUI's own lock
//! fallback makes the second and later ones observers.
//!
//! This is remote code execution as the user (agents may run with permission
//! checks off), so the safety rules are the point of the module:
//!
//! 1. every request for the page or the websocket needs a credential: by
//!    default a random per-start token (`?token=`, or as the Basic password),
//!    or `--password` (HTTP Basic, any username). Both are compared as SHA-256
//!    digests in constant time, and repeated failures from one peer address
//!    are answered with 429 and a growing back-off ([`FailureTracker`]).
//!    `--no-auth` turns this off and is only accepted on a named interface;
//! 2. the `Host` header must match the bind ([`HostPolicy`]), which defeats
//!    DNS rebinding: a hostile name resolving to 127.0.0.1 would otherwise
//!    make `Origin` and `Host` agree;
//! 3. a websocket upgrade whose `Origin` is not the request's own `Host` is
//!    rejected, so a page on another site cannot drive `ws://127.0.0.1`
//!    ([`origin_allowed`]);
//! 4. incoming websocket messages are size-bounded ([`MAX_MESSAGE_BYTES`]),
//!    sessions are capped ([`MAX_SESSIONS`]), and every response carries
//!    anti-framing headers ([`security_headers`]) so a framed page cannot
//!    sidestep rule 3.
//!
//! The static assets (xterm.js, the CSS, licences) carry no secret and need no
//! credential, which is what lets a token-bearing `/` load them.

pub mod pty;

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, RawQuery, Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use orch_core::error::{Error, Result};
use portable_pty::PtySize;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio::sync::mpsc::error::TrySendError;
use tokio_util::sync::CancellationToken;

pub use pty::{PtySession, SessionSlot, SpawnSpec};

pub const DEFAULT_BIND: &str = "127.0.0.1:7681";
pub const PASSWORD_ENV: &str = "ORCH_WEB_PASSWORD";

/// Largest websocket message (and frame) the server accepts. Keystrokes and
/// pastes are tiny; the page splits anything larger into chunks of 16 KiB.
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;
pub const MAX_SESSIONS: usize = 8;
/// Shortest `--password` accepted on a non-loopback bind.
pub const MIN_REMOTE_PASSWORD_CHARS: usize = 16;

const DEFAULT_COLS: u16 = 120;
const DEFAULT_ROWS: u16 = 40;
const MAX_DIMENSION: u32 = 500;

const PING_EVERY: Duration = Duration::from_secs(20);
/// A client that has sent nothing (not even a pong) for this long is gone.
const IDLE_LIMIT: Duration = Duration::from_secs(60);
const SEND_TIMEOUT: Duration = Duration::from_secs(30);

const FREE_FAILURES: u32 = 5;
const BASE_BLOCK: Duration = Duration::from_secs(30);
const MAX_BLOCK: Duration = Duration::from_secs(15 * 60);
const MAX_TRACKED_PEERS: usize = 4096;

/// How long `run` waits for sessions to wind down on SIGINT/SIGTERM: the
/// children's own grace ([`pty::KILL_GRACE`]), plus a margin.
const DRAIN_WAIT: Duration = Duration::from_secs(17);

/// How a client proves it may use the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Auth {
    /// `--no-auth`.
    Open,
    /// The generated default. Accepted as `?token=` or as the Basic password.
    Token(String),
    /// `--password`. Accepted as the Basic password only.
    Password(String),
}

/// The raw inputs of `orch web`, before the safety rules are applied.
#[derive(Debug, Default)]
pub struct WebOptions<'a> {
    pub bind: &'a str,
    /// `--password`.
    pub password: Option<String>,
    /// The value of `ORCH_WEB_PASSWORD`, if set.
    pub env_password: Option<String>,
    pub no_auth: bool,
    pub allow_hosts: &'a [String],
    pub observe: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebArgs {
    pub bind: SocketAddr,
    pub auth: Auth,
    pub allow_hosts: Vec<String>,
    pub observe: bool,
}

impl WebArgs {
    /// Apply the bind/credential rules. An empty password counts as unset, so
    /// an empty `ORCH_WEB_PASSWORD` cannot silently change the mode.
    pub fn parse(options: WebOptions<'_>) -> std::result::Result<Self, String> {
        let bind: SocketAddr = options.bind.parse().map_err(|_| {
            format!(
                "Invalid --bind \"{}\". Expected <addr>:<port>",
                options.bind
            )
        })?;
        let env_password = options.env_password.filter(|password| !password.is_empty());
        let password = options.password.filter(|password| !password.is_empty());

        let auth = if options.no_auth {
            if password.is_some() || env_password.is_some() {
                return Err(format!(
                    "--no-auth conflicts with a password: unset {PASSWORD_ENV} or drop --no-auth"
                ));
            }
            if bind.ip().is_unspecified() {
                return Err(format!(
                    "Refusing --no-auth on the wildcard address {bind}: bind a specific interface IP"
                ));
            }
            Auth::Open
        } else if let Some(password) = password.or(env_password) {
            if !bind.ip().is_loopback() && password.chars().count() < MIN_REMOTE_PASSWORD_CHARS {
                return Err(format!(
                    "The password must be at least {MIN_REMOTE_PASSWORD_CHARS} characters when listening on {bind}"
                ));
            }
            Auth::Password(password)
        } else {
            Auth::Token(new_token())
        };

        let allow_hosts = options
            .allow_hosts
            .iter()
            .map(|host| normalize_allowed_host(host))
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(WebArgs {
            bind,
            auth,
            allow_hosts,
            observe: options.observe,
        })
    }

    /// Stderr warnings to print on every start.
    pub fn warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        if self.auth == Auth::Open {
            warnings.push(
                "WARNING: --no-auth. Anyone who can reach this port can run code as you, \
                 and the session is unencrypted. Restrict who can reach it with a firewall, \
                 Tailscale ACLs or an SSH tunnel."
                    .to_owned(),
            );
        }
        if !self.bind.ip().is_loopback() {
            warnings.push(format!(
                "WARNING: listening on {}. The connection is unencrypted HTTP, so the \
                 credentials and the whole session are readable on the network, and anyone \
                 holding the credentials can run code as you. Prefer an SSH tunnel or a TLS \
                 reverse proxy.",
                self.bind
            ));
        }
        warnings
    }
}

fn new_token() -> String {
    const HEX: [char; 16] = [
        '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'a', 'b', 'c', 'd', 'e', 'f',
    ];
    // 32 hex characters is 128 bits, from an OS-seeded CSPRNG.
    nanoid::nanoid!(32, &HEX)
}

fn normalize_allowed_host(host: &str) -> std::result::Result<String, String> {
    let name = host.trim().to_ascii_lowercase();
    let name = name.strip_suffix('.').unwrap_or(&name);
    if name.is_empty() || name.contains([':', '/', '@', '*', ' ']) {
        return Err(format!(
            "Invalid --allow-host \"{host}\": give a bare host name, with no port or wildcard"
        ));
    }
    Ok(name.to_owned())
}

/// Which `Host` headers are served.
///
/// A loopback bind accepts loopback names and IPs. Any other bind accepts IP
/// literals (a name an attacker controls can rebind to anything, an IP
/// cannot). Either way, `--allow-host` names are accepted by exact match.
#[derive(Debug, Clone)]
pub struct HostPolicy {
    loopback_bind: bool,
    allowed: Vec<String>,
}

impl HostPolicy {
    pub fn new(loopback_bind: bool, allowed: Vec<String>) -> Self {
        HostPolicy {
            loopback_bind,
            allowed,
        }
    }

    pub fn allows(&self, host: &str) -> bool {
        let name = host_name(host);
        if self.allowed.contains(&name) {
            return true;
        }
        let ip = name.parse::<IpAddr>().ok();
        if self.loopback_bind {
            name == "localhost" || ip.is_some_and(|ip| ip.is_loopback())
        } else {
            ip.is_some()
        }
    }
}

/// The host of a `Host` header: port removed, lowercased, one trailing dot
/// stripped, IPv6 brackets removed.
fn host_name(host: &str) -> String {
    let name = match host.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or(rest),
        None => host.rsplit_once(':').map_or(host, |(name, _)| name),
    };
    let name = name.to_ascii_lowercase();
    name.strip_suffix('.').unwrap_or(&name).to_owned()
}

/// Consecutive-failure back-off per peer address.
#[derive(Debug, Default)]
pub struct FailureTracker {
    peers: Mutex<HashMap<IpAddr, Failures>>,
}

#[derive(Debug, Default)]
struct Failures {
    count: u32,
    blocked_until: Option<Instant>,
}

impl FailureTracker {
    /// How much longer `peer` is blocked, if it is.
    pub fn blocked_for(&self, peer: IpAddr, now: Instant) -> Option<Duration> {
        let peers = self.peers.lock().unwrap_or_else(|e| e.into_inner());
        let until = peers.get(&peer)?.blocked_until?;
        until.checked_duration_since(now).filter(|d| !d.is_zero())
    }

    /// Record a failed attempt; returns the new block, if it started one.
    /// Blocks start at the fifth consecutive failure and double after that.
    pub fn record_failure(&self, peer: IpAddr, now: Instant) -> Option<Duration> {
        let mut peers = self.peers.lock().unwrap_or_else(|e| e.into_inner());
        if peers.len() >= MAX_TRACKED_PEERS {
            peers.retain(|_, f| f.blocked_until.is_some_and(|until| until > now));
        }
        let failures = peers.entry(peer).or_default();
        failures.count = failures.count.saturating_add(1);
        let over = failures.count.checked_sub(FREE_FAILURES)?;
        let block = BASE_BLOCK
            .saturating_mul(2u32.saturating_pow(over.min(16)))
            .min(MAX_BLOCK);
        failures.blocked_until = Some(now + block);
        Some(block)
    }

    pub fn record_success(&self, peer: IpAddr) {
        self.peers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&peer);
    }
}

/// The server's shared state.
pub struct WebState {
    secret: Option<Secret>,
    hosts: HostPolicy,
    spawn: SpawnSpec,
    failures: FailureTracker,
    slots: Arc<AtomicUsize>,
    shutdown: CancellationToken,
}

struct Secret {
    digest: [u8; 32],
    accepts_query_token: bool,
}

impl Secret {
    fn matches(&self, presented: &[u8]) -> bool {
        // Digests are fixed-length, so neither the content nor the length of
        // the real secret shows in the timing.
        let presented: [u8; 32] = Sha256::digest(presented).into();
        presented.ct_eq(&self.digest).into()
    }
}

impl WebState {
    pub fn new(auth: &Auth, hosts: HostPolicy, spawn: SpawnSpec) -> Self {
        let secret = |value: &str, accepts_query_token| {
            Some(Secret {
                digest: Sha256::digest(value.as_bytes()).into(),
                accepts_query_token,
            })
        };
        WebState {
            secret: match auth {
                Auth::Open => None,
                Auth::Token(token) => secret(token, true),
                Auth::Password(password) => secret(password, false),
            },
            hosts,
            spawn,
            failures: FailureTracker::default(),
            slots: Arc::new(AtomicUsize::new(0)),
            shutdown: CancellationToken::new(),
        }
    }

    /// Sessions still running or shutting down.
    pub fn live_sessions(&self) -> usize {
        self.slots.load(Ordering::SeqCst)
    }

    /// Ask every session to close.
    pub fn shutdown(&self) {
        self.shutdown.cancel();
    }
}

/// A JSON text frame from the browser. Anything that does not parse, or has an
/// out-of-range size, is ignored by [`parse_resize`].
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Control {
    Resize { cols: u32, rows: u32 },
}

/// The `(cols, rows)` of a valid resize message, `None` for anything else.
pub fn parse_resize(text: &str) -> Option<(u16, u16)> {
    let Control::Resize { cols, rows } = serde_json::from_str(text).ok()?;
    Some((valid_dimension(cols)?, valid_dimension(rows)?))
}

fn valid_dimension(value: u32) -> Option<u16> {
    (1..=MAX_DIMENSION)
        .contains(&value)
        .then(|| u16::try_from(value).ok())
        .flatten()
}

pub fn router(state: Arc<WebState>) -> Router {
    // Credentials are decided by the router: only these two routes are layered.
    let authed = Router::new()
        .route(
            "/",
            get(|| async { Html(include_str!("assets/index.html")) }),
        )
        .route("/ws", get(ws_handler))
        .route_layer(middleware::from_fn_with_state(state.clone(), auth));
    // Compiled in and immutable; they carry no secret.
    let assets = Router::new()
        .route(
            "/xterm.js",
            get(|| asset("text/javascript", include_str!("assets/xterm.js"))),
        )
        .route(
            "/xterm.css",
            get(|| asset("text/css", include_str!("assets/xterm.css"))),
        )
        .route(
            "/addon-fit.js",
            get(|| asset("text/javascript", include_str!("assets/addon-fit.js"))),
        )
        .route(
            "/licenses/xterm.txt",
            get(|| asset("text/plain", include_str!("assets/LICENSE-xterm.txt"))),
        )
        .route(
            "/licenses/addon-fit.txt",
            get(|| asset("text/plain", include_str!("assets/LICENSE-addon-fit.txt"))),
        );
    authed
        .merge(assets)
        .layer(middleware::from_fn_with_state(state.clone(), host_guard))
        .layer(middleware::map_response(security_headers))
        .with_state(state)
}

async fn asset(content_type: &'static str, body: &'static str) -> impl IntoResponse {
    (
        [
            (
                header::CONTENT_TYPE,
                format!("{content_type}; charset=utf-8"),
            ),
            (
                header::CACHE_CONTROL,
                "public, max-age=31536000, immutable".to_owned(),
            ),
        ],
        body,
    )
}

/// Headers for every response, including rejections. `no-store` is only a
/// default: the static assets set their own long cache. Without the framing
/// ones, a hostile page could embed this one and drive it from the user's
/// own origin.
async fn security_headers(mut response: Response) -> Response {
    const HEADERS: [(HeaderName, &str); 4] = [
        (header::X_FRAME_OPTIONS, "DENY"),
        (
            header::CONTENT_SECURITY_POLICY,
            "default-src 'none'; script-src 'self' 'unsafe-inline'; \
             style-src 'self' 'unsafe-inline'; connect-src 'self'; frame-ancestors 'none'",
        ),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (header::REFERRER_POLICY, "no-referrer"),
    ];
    let headers = response.headers_mut();
    for (name, value) in HEADERS {
        headers.insert(name, HeaderValue::from_static(value));
    }
    headers
        .entry(header::CACHE_CONTROL)
        .or_insert(HeaderValue::from_static("no-store"));
    response
}

fn reply(status: StatusCode, body: &'static str) -> Response {
    (status, body).into_response()
}

/// Host policy for every route.
async fn host_guard(
    State(state): State<Arc<WebState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: Request,
    next: Next,
) -> Response {
    let host = header_str(request.headers(), header::HOST);
    if !host.is_some_and(|host| state.hosts.allows(host)) {
        eprintln!("orch web: rejected {peer}: Host header {host:?} is not allowed");
        return reply(StatusCode::FORBIDDEN, "host not allowed\n");
    }
    next.run(request).await
}

/// Credentials for the page and the websocket.
///
/// A correct credential always passes, whatever the peer's history: behind a
/// reverse proxy every client shares one address, and a back-off that applied
/// first would let an attacker lock the owner out. The back-off only decides
/// how a *rejected* request is answered. (`X-Forwarded-For` is not trusted.)
async fn auth(
    State(state): State<Arc<WebState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: Request,
    next: Next,
) -> Response {
    let Some(secret) = &state.secret else {
        return next.run(request).await;
    };
    let ip = peer.ip();
    let presented = presented_credential(request.headers(), request.uri().query(), secret);
    if presented.as_deref().is_some_and(|p| secret.matches(p)) {
        state.failures.record_success(ip);
        return next.run(request).await;
    }

    if let Some(wait) = state.failures.blocked_for(ip, Instant::now()) {
        let mut response = reply(StatusCode::TOO_MANY_REQUESTS, "too many failed attempts\n");
        if let Ok(value) = HeaderValue::from_str(&wait.as_secs().max(1).to_string()) {
            response.headers_mut().insert(header::RETRY_AFTER, value);
        }
        return response;
    }
    if presented.is_none() {
        // A browser's first request to the page has no credential yet; only
        // the websocket is worth a log line.
        if request.uri().path() == "/ws" {
            eprintln!("orch web: websocket from {peer} without credentials");
        }
        return (
            StatusCode::UNAUTHORIZED,
            [(
                header::WWW_AUTHENTICATE,
                r#"Basic realm="orch", charset="UTF-8""#,
            )],
            "authentication required\n",
        )
            .into_response();
    }
    match state.failures.record_failure(ip, Instant::now()) {
        Some(block) => eprintln!(
            "orch web: bad credentials from {peer}; blocking it for {}s",
            block.as_secs()
        ),
        None => eprintln!("orch web: bad credentials from {peer}"),
    }
    reply(StatusCode::UNAUTHORIZED, "invalid credentials\n")
}

fn header_str(headers: &HeaderMap, name: HeaderName) -> Option<&str> {
    headers.get(name)?.to_str().ok()
}

/// The credential the request carries, if it carries one: the `token` query
/// parameter (token mode only), else the password of a Basic header.
fn presented_credential(
    headers: &HeaderMap,
    query: Option<&str>,
    secret: &Secret,
) -> Option<Vec<u8>> {
    if secret.accepts_query_token
        && let Some(token) = query.and_then(|query| query_param(query, "token"))
    {
        return Some(token.as_bytes().to_vec());
    }
    basic_password(headers)
}

/// The password of an `Authorization: Basic` header (any username). A header
/// that is present but malformed yields an empty credential, which fails the
/// check and so counts as an attempt.
fn basic_password(headers: &HeaderMap) -> Option<Vec<u8>> {
    let value = header_str(headers, header::AUTHORIZATION)?;
    let decoded = value
        .split_once(' ')
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("basic"))
        .and_then(|(_, encoded)| STANDARD.decode(encoded.trim()).ok());
    let password = decoded.and_then(|decoded| {
        let colon = decoded.iter().position(|byte| *byte == b':')?;
        Some(decoded[colon + 1..].to_vec())
    });
    Some(password.unwrap_or_default())
}

/// The raw value of a query parameter. The values here (hex tokens, numbers)
/// never need percent-decoding.
fn query_param<'a>(query: &'a str, name: &str) -> Option<&'a str> {
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find_map(|(key, value)| (key == name).then_some(value))
}

/// Whether a websocket upgrade is same-origin. Browsers always send `Origin`
/// on a websocket handshake, so a missing one means a non-browser client,
/// which can forge any header anyway and is gated by the credential instead.
pub fn origin_allowed(origin: Option<&str>, host: Option<&str>) -> bool {
    let Some(origin) = origin else {
        return true;
    };
    let Some(host) = host else {
        return false;
    };
    let Some((_scheme, authority)) = origin.split_once("://") else {
        return false; // "null" and garbage
    };
    let authority = authority.split('/').next().unwrap_or(authority);
    authority.eq_ignore_ascii_case(host)
}

/// `cols` and `rows` from the query; anything unusable falls back to the
/// default rather than failing the upgrade.
fn initial_size(query: Option<&str>) -> PtySize {
    let dimension = |name, default| {
        query
            .and_then(|query| query_param(query, name))
            .and_then(|value| value.parse::<u32>().ok())
            .and_then(valid_dimension)
            .unwrap_or(default)
    };
    PtySize {
        cols: dimension("cols", DEFAULT_COLS),
        rows: dimension("rows", DEFAULT_ROWS),
        pixel_width: 0,
        pixel_height: 0,
    }
}

async fn ws_handler(
    State(state): State<Arc<WebState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    let origin = header_str(&headers, header::ORIGIN);
    if !origin_allowed(origin, header_str(&headers, header::HOST)) {
        eprintln!("orch web: rejected {peer}: cross-origin websocket from {origin:?}");
        return reply(StatusCode::FORBIDDEN, "cross-origin websocket rejected\n");
    }
    let Some(slot) = SessionSlot::acquire(&state.slots, MAX_SESSIONS) else {
        eprintln!("orch web: rejected {peer}: {MAX_SESSIONS} sessions already open");
        return reply(StatusCode::SERVICE_UNAVAILABLE, "too many sessions\n");
    };
    let size = initial_size(query.as_deref());
    upgrade
        .max_message_size(MAX_MESSAGE_BYTES)
        .max_frame_size(MAX_MESSAGE_BYTES)
        .on_upgrade(move |socket| bridge(socket, state, size, slot, peer))
}

/// Send with a deadline, so a browser that vanished mid-stream cannot hold a
/// session (and its `orch tui`) forever.
async fn send(socket: &mut WebSocket, message: Message) -> bool {
    matches!(
        tokio::time::timeout(SEND_TIMEOUT, socket.send(message)).await,
        Ok(Ok(()))
    )
}

fn close_message(code: u16, reason: &'static str) -> Message {
    Message::Close(Some(CloseFrame {
        code,
        reason: reason.into(),
    }))
}

/// Pump bytes between one websocket and one pty until either side ends. The
/// pty (and so the child) is torn down when this returns.
async fn bridge(
    mut socket: WebSocket,
    state: Arc<WebState>,
    size: PtySize,
    slot: SessionSlot,
    peer: SocketAddr,
) {
    let mut session = match PtySession::spawn(&state.spawn, size, slot) {
        Ok(session) => session,
        Err(err) => {
            eprintln!(
                "orch web: could not start {} for {peer}: {err}",
                state.spawn.program.display()
            );
            let _ = send(
                &mut socket,
                Message::Binary(
                    b"\r\n[orch web] could not start the TUI. See the server log.\r\n"
                        .to_vec()
                        .into(),
                ),
            )
            .await;
            let _ = send(&mut socket, close_message(1011, "spawn failed")).await;
            return;
        }
    };
    eprintln!("orch web: session started for {peer}");

    let mut ping = tokio::time::interval(PING_EVERY);
    ping.tick().await; // the first tick is immediate
    let mut last_heard = Instant::now();

    let (code, reason) = loop {
        tokio::select! {
            () = state.shutdown.cancelled() => break (1001, "server shutting down"),
            _ = ping.tick() => {
                if last_heard.elapsed() > IDLE_LIMIT {
                    eprintln!("orch web: {peer} stopped answering; closing its session");
                    break (1001, "timed out");
                }
                if !send(&mut socket, Message::Ping(Vec::new().into())).await {
                    break (1001, "send failed");
                }
            }
            output = session.output.recv() => match output {
                Some(bytes) => {
                    if !send(&mut socket, Message::Binary(bytes.into())).await {
                        break (1001, "send failed");
                    }
                }
                None => break (1000, "session ended"),
            },
            message = socket.recv() => {
                last_heard = Instant::now();
                match message {
                    // Never wait on the child here: if it is not reading its
                    // input, output draining and close detection must go on.
                    Some(Ok(Message::Binary(bytes))) => match session.input.try_send(bytes.to_vec()) {
                        Ok(()) => {}
                        Err(TrySendError::Full(_)) => break (1013, "input backlog"),
                        Err(TrySendError::Closed(_)) => break (1000, "session ended"),
                    },
                    Some(Ok(Message::Text(text))) => match parse_resize(&text) {
                        Some((cols, rows)) => session.resize(cols, rows),
                        None => tracing::debug!("web: ignored a malformed control message"),
                    },
                    Some(Ok(Message::Close(_))) | None => break (1000, "closed"),
                    Some(Ok(_)) => {} // ping/pong are answered by axum
                    Some(Err(err)) => {
                        tracing::debug!(%err, "web: websocket error");
                        if err.to_string().contains("too long") {
                            break (1009, "message too big");
                        }
                        break (1002, "websocket error");
                    }
                }
            }
        }
    };

    drop(session);
    let _ = tokio::time::timeout(
        Duration::from_secs(5),
        socket.send(close_message(code, reason)),
    )
    .await;
    eprintln!("orch web: session for {peer} ended ({reason})");
}

/// Resolves on Ctrl-C or, on unix, SIGTERM.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = terminate.recv() => {}
                }
                return;
            }
            Err(err) => tracing::warn!(%err, "cannot listen for SIGTERM"),
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

/// `orch web`: serve until interrupted, then give open sessions their
/// shutdown grace (so their agents are stopped, not orphaned) and exit.
pub async fn run(args: WebArgs, cwd: PathBuf) -> Result<ExitCode> {
    let program = std::env::current_exe()
        .map_err(|err| Error::io("locate the orch executable", &cwd, err))?;
    let mut tui_args = vec!["tui".into()];
    if args.observe {
        tui_args.push("--observe".into());
    }
    let state = Arc::new(WebState::new(
        &args.auth,
        HostPolicy::new(args.bind.ip().is_loopback(), args.allow_hosts.clone()),
        SpawnSpec {
            program,
            args: tui_args,
            cwd: cwd.clone(),
        },
    ));

    let listener = tokio::net::TcpListener::bind(args.bind)
        .await
        .map_err(|err| Error::io(format!("listen on {}", args.bind), &cwd, err))?;
    let addr = listener
        .local_addr()
        .map_err(|err| Error::io("read the listen address", &cwd, err))?;

    for warning in args.warnings() {
        eprintln!("{warning}");
    }
    match &args.auth {
        Auth::Token(token) => {
            println!("orch web listening on http://{addr}/?token={token}");
            if !addr.ip().is_loopback() {
                println!(
                    "The token also works as the HTTP Basic password (any username). The ?token= \
                     form ends up in proxy access logs and browser history; prefer Basic auth."
                );
            }
        }
        Auth::Password(_) => {
            println!("orch web listening on http://{addr}/ (HTTP Basic, any username)");
        }
        Auth::Open => println!("orch web listening on http://{addr}/ (no authentication)"),
    }

    let service = router(Arc::clone(&state)).into_make_service_with_connect_info::<SocketAddr>();
    tokio::select! {
        served = axum::serve(listener, service) => {
            served.map_err(|err| Error::io("serve", &cwd, err))?;
        }
        () = shutdown_signal() => {}
    }

    state.shutdown();
    let waiting = state.live_sessions();
    if waiting > 0 {
        eprintln!(
            "orch web: shutting down, waiting for {waiting} session(s)... (signal again to exit now)"
        );
    }
    let second_signal = shutdown_signal();
    tokio::pin!(second_signal);
    let deadline = Instant::now() + DRAIN_WAIT;
    while state.live_sessions() > 0 && Instant::now() < deadline {
        tokio::select! {
            () = &mut second_signal => {
                eprintln!("orch web: second signal, exiting now");
                break;
            }
            () = tokio::time::sleep(Duration::from_millis(50)) => {}
        }
    }
    let left = state.live_sessions();
    if left > 0 {
        eprintln!("orch web: exiting with {left} session(s) still shutting down");
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options<'a>(bind: &'a str) -> WebOptions<'a> {
        WebOptions {
            bind,
            ..WebOptions::default()
        }
    }

    #[test]
    fn the_default_is_a_random_token_on_any_bind() {
        for bind in ["127.0.0.1:7681", "0.0.0.0:7681", "192.168.1.5:7681"] {
            let args = WebArgs::parse(options(bind)).expect(bind);
            let Auth::Token(token) = args.auth else {
                panic!("{bind}: expected a token");
            };
            assert_eq!(token.len(), 32);
            assert!(token.chars().all(|c| c.is_ascii_hexdigit()), "{token}");
        }
        let a = WebArgs::parse(options("127.0.0.1:1")).expect("a");
        let b = WebArgs::parse(options("127.0.0.1:1")).expect("b");
        assert_ne!(a.auth, b.auth, "tokens must differ per start");
    }

    #[test]
    fn passwords_need_sixteen_characters_off_loopback_only() {
        let with = |bind, password: &str| {
            WebArgs::parse(WebOptions {
                password: Some(password.to_owned()),
                ..options(bind)
            })
        };
        assert!(with("127.0.0.1:1", "short").is_ok());
        assert!(with("0.0.0.0:1", "short").is_err());
        assert!(with("0.0.0.0:1", "fifteen-chars-xx").is_ok()); // 16
        assert!(with("0.0.0.0:1", "fifteen-chars-x").is_err()); // 15
        // The env variable counts the same, and an empty one is unset.
        let env = WebArgs::parse(WebOptions {
            env_password: Some("short".into()),
            ..options("10.0.0.1:1")
        });
        assert!(env.is_err());
        let empty = WebArgs::parse(WebOptions {
            env_password: Some(String::new()),
            ..options("10.0.0.1:1")
        })
        .expect("empty env is unset");
        assert!(matches!(empty.auth, Auth::Token(_)));
    }

    #[test]
    fn no_auth_rules() {
        let no_auth = |bind, env: Option<&str>| {
            WebArgs::parse(WebOptions {
                no_auth: true,
                env_password: env.map(str::to_owned),
                ..options(bind)
            })
        };
        assert_eq!(
            no_auth("127.0.0.1:1", None).expect("loopback").auth,
            Auth::Open
        );
        assert!(
            no_auth("192.168.1.5:1", None).is_ok(),
            "a named interface is fine"
        );
        assert!(no_auth("0.0.0.0:1", None).is_err());
        assert!(no_auth("[::]:1", None).is_err());
        assert!(no_auth("127.0.0.1:1", Some("pw")).is_err());
        assert!(
            no_auth("127.0.0.1:1", Some("")).is_ok(),
            "empty env is unset"
        );
        let conflict = WebArgs::parse(WebOptions {
            no_auth: true,
            password: Some("pw".into()),
            ..options("127.0.0.1:1")
        });
        assert!(conflict.is_err());
    }

    #[test]
    fn warnings_cover_cleartext_and_no_auth() {
        let quiet = WebArgs::parse(options("127.0.0.1:1")).expect("loopback");
        assert!(quiet.warnings().is_empty());
        let remote = WebArgs::parse(options("10.0.0.1:1")).expect("remote");
        assert!(remote.warnings()[0].contains("unencrypted"));
        let open = WebArgs::parse(WebOptions {
            no_auth: true,
            ..options("10.0.0.1:1")
        })
        .expect("open");
        assert_eq!(open.warnings().len(), 2);
        assert!(open.warnings()[0].contains("--no-auth"));
    }

    #[test]
    fn a_bind_without_a_port_is_rejected() {
        assert!(WebArgs::parse(options("127.0.0.1")).is_err());
        assert!(WebArgs::parse(options("localhost:7681")).is_err());
    }

    #[test]
    fn same_origin_upgrades_pass_and_cross_origin_ones_do_not() {
        let host = Some("127.0.0.1:7681");
        assert!(origin_allowed(Some("http://127.0.0.1:7681"), host));
        assert!(origin_allowed(Some("https://127.0.0.1:7681"), host));
        assert!(origin_allowed(
            Some("http://LOCALHOST:7681"),
            Some("localhost:7681")
        ));
        assert!(
            origin_allowed(None, host),
            "non-browser clients send no Origin"
        );

        assert!(!origin_allowed(Some("http://evil.example"), host));
        assert!(!origin_allowed(Some("http://127.0.0.1:7682"), host));
        assert!(!origin_allowed(Some("http://127.0.0.1"), host));
        assert!(!origin_allowed(
            Some("http://127.0.0.1:7681.evil.example"),
            host
        ));
        assert!(!origin_allowed(Some("null"), host));
        assert!(!origin_allowed(Some("http://127.0.0.1:7681"), None));
    }

    #[test]
    fn host_policy_follows_the_bind() {
        let loopback = HostPolicy::new(true, vec!["orch.example".into()]);
        for host in [
            "localhost",
            "LOCALHOST:7681",
            "localhost.:1",
            "127.0.0.1:7681",
            "[::1]:7681",
            "orch.example:80",
            "ORCH.example.",
        ] {
            assert!(loopback.allows(host), "{host}");
        }
        for host in [
            "evil.example",
            "10.0.0.1:7681",
            "app.localhost",
            "xorch.example",
            "sub.orch.example",
        ] {
            assert!(!loopback.allows(host), "{host}");
        }

        let remote = HostPolicy::new(false, vec!["orch.example".into()]);
        for host in [
            "10.0.0.1:7681",
            "[fd00::1]:7681",
            "127.0.0.1",
            "orch.example:7681",
        ] {
            assert!(remote.allows(host), "{host}");
        }
        for host in ["localhost:7681", "evil.example", "10.0.0.1.evil.example"] {
            assert!(!remote.allows(host), "{host}");
        }
    }

    #[test]
    fn allow_host_names_are_normalised_and_must_be_bare() {
        assert_eq!(
            normalize_allowed_host("Orch.Example.").as_deref(),
            Ok("orch.example")
        );
        for bad in ["", "a:80", "*.example", "a/b", "user@host", "a b"] {
            assert!(normalize_allowed_host(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn resize_messages_are_validated() {
        assert_eq!(
            parse_resize(r#"{"type":"resize","cols":80,"rows":24}"#),
            Some((80, 24))
        );
        assert_eq!(
            parse_resize(r#"{"type":"resize","cols":500,"rows":1}"#),
            Some((500, 1))
        );
        for bad in [
            r#"{"type":"resize","cols":0,"rows":24}"#,
            r#"{"type":"resize","cols":80,"rows":501}"#,
            r#"{"type":"resize","cols":-1,"rows":24}"#,
            r#"{"type":"resize","cols":"80","rows":24}"#,
            r#"{"type":"resize","cols":80}"#,
            r#"{"type":"explode","cols":80,"rows":24}"#,
            r#"{"cols":80,"rows":24}"#,
            "not json",
            "",
        ] {
            assert_eq!(parse_resize(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_initial_size_falls_back_instead_of_failing() {
        let size = |query| {
            let size = initial_size(query);
            (size.cols, size.rows)
        };
        assert_eq!(size(Some("cols=80&rows=24")), (80, 24));
        assert_eq!(size(Some("token=ab&cols=90&rows=30")), (90, 30));
        assert_eq!(size(None), (120, 40));
        assert_eq!(size(Some("cols=abc&rows=-3")), (120, 40));
        assert_eq!(size(Some("cols=501&rows=0")), (120, 40));
    }

    #[test]
    fn credentials_are_checked_as_token_or_basic_password() {
        let secret = |query_token| Secret {
            digest: Sha256::digest(b"s3cret").into(),
            accepts_query_token: query_token,
        };
        let basic = |credentials: &str| {
            let mut headers = HeaderMap::new();
            let value = format!("Basic {}", STANDARD.encode(credentials));
            headers.insert(header::AUTHORIZATION, value.parse().expect("header"));
            headers
        };
        let check = |headers: &HeaderMap, query, query_token| {
            let secret = secret(query_token);
            presented_credential(headers, query, &secret).map(|p| secret.matches(&p))
        };
        let none = HeaderMap::new();

        assert_eq!(check(&none, Some("token=s3cret"), true), Some(true));
        assert_eq!(check(&none, Some("a=1&token=nope"), true), Some(false));
        assert_eq!(
            check(&none, Some("token=s3cret"), false),
            None,
            "password mode ignores ?token="
        );
        assert_eq!(check(&none, None, true), None);
        assert_eq!(check(&basic("anyone:s3cret"), None, false), Some(true));
        assert_eq!(check(&basic(":s3cret"), None, true), Some(true));
        assert_eq!(check(&basic("a:b:c"), None, false), Some(false));
        assert_eq!(check(&basic("anyone:wrong"), None, false), Some(false));
        assert_eq!(check(&basic("nocolon"), None, false), Some(false));

        let mut bearer = HeaderMap::new();
        bearer.insert(
            header::AUTHORIZATION,
            "Bearer s3cret".parse().expect("header"),
        );
        assert_eq!(check(&bearer, None, false), Some(false));
        let mut garbage = HeaderMap::new();
        garbage.insert(header::AUTHORIZATION, "Basic !!!".parse().expect("header"));
        assert_eq!(check(&garbage, None, false), Some(false));
    }

    #[test]
    fn failures_back_off_and_double_up_to_a_cap() {
        let tracker = FailureTracker::default();
        let peer: IpAddr = "10.0.0.9".parse().expect("ip");
        let other: IpAddr = "10.0.0.10".parse().expect("ip");
        let t0 = Instant::now();

        for _ in 0..4 {
            assert_eq!(tracker.record_failure(peer, t0), None);
        }
        assert_eq!(tracker.blocked_for(peer, t0), None);
        assert_eq!(
            tracker.record_failure(peer, t0),
            Some(Duration::from_secs(30))
        );
        assert!(tracker.blocked_for(peer, t0).is_some());
        assert_eq!(tracker.blocked_for(other, t0), None, "per peer");
        assert_eq!(
            tracker.blocked_for(peer, t0 + Duration::from_secs(31)),
            None
        );
        assert_eq!(
            tracker.record_failure(peer, t0),
            Some(Duration::from_secs(60))
        );
        assert_eq!(
            tracker.record_failure(peer, t0),
            Some(Duration::from_secs(120))
        );
        for _ in 0..40 {
            tracker.record_failure(peer, t0);
        }
        assert_eq!(tracker.record_failure(peer, t0), Some(MAX_BLOCK));

        tracker.record_success(peer);
        assert_eq!(tracker.blocked_for(peer, t0), None);
        assert_eq!(
            tracker.record_failure(peer, t0),
            None,
            "success resets the count"
        );
    }

    #[test]
    fn the_session_cap_counts_slots_until_they_drop() {
        let live = Arc::new(AtomicUsize::new(0));
        let a = SessionSlot::acquire(&live, 2).expect("first");
        let _b = SessionSlot::acquire(&live, 2).expect("second");
        assert!(SessionSlot::acquire(&live, 2).is_none());
        assert_eq!(live.load(Ordering::SeqCst), 2, "a refused slot is released");
        drop(a);
        assert!(SessionSlot::acquire(&live, 2).is_some());
    }
}
