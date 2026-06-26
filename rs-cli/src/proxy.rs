//! Loopback proxy that adds MicroVM auth headers for browser HTTP/WS traffic.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::RwLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context, Result};
use aws_sdk_lambdamicrovms::Client as MicrovmClient;
use aws_sdk_lambdamicrovms::types::PortSpecification;
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::header::{HeaderMap, HeaderName, HeaderValue};
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::Role;

use crate::lifecycle::{host_of, microvm_endpoint, microvm_state, poll_microvm_state};
use crate::poll::PollOpts;
use crate::state::State;

const AUTH_TOKEN_KEY: &str = "X-aws-proxy-auth";
const TOKEN_TTL_MINUTES: i32 = 30;
const CONTROL_COOKIE_NAME: &str = "hellbox_control";
const CONTROL_COOKIE_PREFIX: &str = "hellbox_control=";

#[cfg(test)]
const AUTH_HEADER: &str = "x-aws-proxy-auth";
#[cfg(test)]
const PORT_HEADER: &str = "x-aws-proxy-port";

/// Live WebSocket sessions for idle detection.
#[derive(Default)]
pub struct ProxyActivity {
    sessions: AtomicUsize,
}

impl ProxyActivity {
    pub fn active(&self) -> usize {
        self.sessions.load(Ordering::Relaxed)
    }
    fn enter(&self) {
        self.sessions.fetch_add(1, Ordering::Relaxed);
    }
    fn leave(&self) {
        self.sessions.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Upstream host/token, both mutable after resume.
#[derive(Clone)]
pub struct Upstream {
    host: Arc<RwLock<String>>,
    auth_token: Arc<RwLock<String>>,
}

impl Upstream {
    pub fn new(host: String, auth_token: String) -> Self {
        Self {
            host: Arc::new(RwLock::new(host)),
            auth_token: Arc::new(RwLock::new(auth_token)),
        }
    }
    fn host(&self) -> String {
        self.host.read().expect("upstream host lock").clone()
    }
    fn token(&self) -> String {
        self.auth_token.read().expect("upstream token lock").clone()
    }
    fn set(&self, host: String, auth_token: String) {
        *self.host.write().expect("upstream host lock") = host;
        *self.auth_token.write().expect("upstream token lock") = auth_token;
    }
}

/// Control-plane state for the injected browser buttons.
pub struct ProxyControl {
    pub microvm: MicrovmClient,
    pub microvm_id: String,
    pub name: String,
    pub token_ports: Vec<i32>,
    pub upstream: Upstream,
    pub control_secret: String,
}

/// Proxy routing and control config.
#[derive(Clone)]
pub struct ProxyConfig {
    pub upstream: Upstream,
    pub upstream_port: i32,
    pub local_port: u16,
    pub routes: Vec<(String, i32)>,
    pub activity: Option<Arc<ProxyActivity>>,
    pub control: Option<Arc<ProxyControl>>,
}

impl ProxyConfig {
    fn port_for(&self, path: &str) -> i32 {
        self.routes
            .iter()
            .find_map(|(prefix, port)| {
                (!prefix.is_empty() && path.starts_with(prefix)).then_some(*port)
            })
            .unwrap_or(self.upstream_port)
    }
}

pub async fn start(cfg: ProxyConfig) -> Result<String> {
    let addr: SocketAddr = ([127, 0, 0, 1], cfg.local_port).into();
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding loopback proxy on {addr}"))?;
    let local = listener.local_addr()?;
    let url = format!("http://{local}");

    let client = reqwest::Client::builder()
        .build()
        .context("building forward HTTP client")?;
    let cfg = Arc::new(cfg);

    tracing::info!(
        target: "hellbox::proxy",
        "Fork B loopback proxy on {local} -> https://{} (port {}, header-injecting)",
        cfg.upstream.host(), cfg.upstream_port
    );

    tokio::spawn(async move {
        loop {
            let (stream, peer) = match listener.accept().await {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(target: "hellbox::proxy", "accept failed: {e:#}");
                    break;
                }
            };
            tracing::debug!(target: "hellbox::proxy", "accepted {peer}");
            let cfg = cfg.clone();
            let client = client.clone();
            tokio::spawn(async move {
                let io = TokioIo::new(stream);
                let svc = service_fn(move |req| handle(req, cfg.clone(), client.clone()));
                if let Err(e) = hyper::server::conn::http1::Builder::new()
                    .serve_connection(io, svc)
                    .with_upgrades()
                    .await
                {
                    tracing::debug!(target: "hellbox::proxy", "connection closed: {e:#}");
                }
            });
        }
    });

    Ok(url)
}

async fn handle(
    req: Request<Incoming>,
    cfg: Arc<ProxyConfig>,
    client: reqwest::Client,
) -> Result<Response<Full<Bytes>>, Infallible> {
    let result = if control_action(req.uri().path()).is_some() {
        handle_control(req, cfg).await
    } else if let Some(reject) = data_plane_rejection(&req, &cfg) {
        Ok(reject)
    } else if is_websocket_upgrade(req.headers()) {
        handle_ws(req, cfg).await
    } else {
        handle_http(req, cfg, client).await
    };
    Ok(result.unwrap_or_else(|e| {
        tracing::warn!(target: "hellbox::proxy", "proxy error: {e:#}");
        Response::builder()
            .status(StatusCode::BAD_GATEWAY)
            .body(Full::new(Bytes::from_static(b"proxy error")))
            .expect("static bad-gateway response")
    }))
}

async fn handle_http(
    req: Request<Incoming>,
    cfg: Arc<ProxyConfig>,
    client: reqwest::Client,
) -> Result<Response<Full<Bytes>>> {
    let (mut parts, body) = req.into_parts();
    let pq = parts
        .uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/")
        .to_string();
    let is_root = parts.uri.path() == "/";
    let is_get = parts.method == hyper::Method::GET;
    let host = cfg.upstream.host();
    let upstream = format!("https://{host}{pq}");

    let method = reqwest::Method::from_bytes(parts.method.as_str().as_bytes()).context("method")?;
    let port = cfg.port_for(parts.uri.path());
    let token = cfg.upstream.token();
    // Avoid data-plane auto-resume on page refresh while suspended.
    if is_root
        && is_get
        && let Some(ctrl) = &cfg.control
        && let Ok(state) = current_state(ctrl).await
        && state != "RUNNING"
    {
        return Ok(html_response_with_control_secret(
            control_only_page(),
            &ctrl.control_secret,
        ));
    }
    // Avoid 304s so panel injection has a body.
    if is_root && is_get && cfg.control.is_some() {
        parts.headers.remove(hyper::header::IF_NONE_MATCH);
        parts.headers.remove(hyper::header::IF_MODIFIED_SINCE);
    }
    let fwd_headers = build_upstream_headers(&parts.headers, &token, port);

    let body_bytes = body
        .collect()
        .await
        .context("reading inbound body")?
        .to_bytes();

    let resp = match client
        .request(method, &upstream)
        .headers(fwd_headers)
        .body(body_bytes)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            // Suspended/unreachable root: serve the local Resume page.
            if is_root
                && is_get
                && let Some(ctrl) = &cfg.control
            {
                return Ok(html_response_with_control_secret(
                    control_only_page(),
                    &ctrl.control_secret,
                ));
            }
            return Err(e).with_context(|| format!("forwarding to {upstream}"));
        }
    };

    let status = resp.status();
    // Suspended/failed root: keep the Resume UI reachable.
    if is_root
        && is_get
        && let Some(ctrl) = &cfg.control
        && status.as_u16() >= 500
    {
        return Ok(html_response_with_control_secret(
            control_only_page(),
            &ctrl.control_secret,
        ));
    }
    let upstream_headers = resp.headers().clone();
    let is_html = upstream_headers
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_ascii_lowercase().contains("text/html"))
        .unwrap_or(false);
    let bytes = resp.bytes().await.context("reading upstream body")?;

    // Add controls without rebuilding the capsule image.
    let injected = is_root && is_get && is_html && cfg.control.is_some();
    let bytes = if injected {
        inject_panel(&bytes)
    } else {
        bytes
    };

    let mut response = Response::new(Full::new(bytes));
    *response.status_mut() =
        StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let out = response.headers_mut();
    for (name, value) in upstream_headers.iter() {
        let n = name.as_str();
        if is_hop_by_hop(n) {
            continue;
        }
        if n.eq_ignore_ascii_case("content-length") {
            continue;
        }
        // Do not cache the injected page.
        if injected
            && (n.eq_ignore_ascii_case("etag")
                || n.eq_ignore_ascii_case("last-modified")
                || n.eq_ignore_ascii_case("cache-control")
                || n.eq_ignore_ascii_case("expires"))
        {
            continue;
        }
        if let (Ok(hn), Ok(hv)) = (
            HeaderName::from_bytes(name.as_str().as_bytes()),
            HeaderValue::from_bytes(value.as_bytes()),
        ) {
            out.append(hn, hv);
        }
    }
    if injected {
        out.insert(
            hyper::header::CACHE_CONTROL,
            HeaderValue::from_static("no-store"),
        );
        if let Some(ctrl) = &cfg.control {
            let cookie = format!(
                "hellbox_control={}; Path=/; HttpOnly; SameSite=Strict; Max-Age=86400",
                ctrl.control_secret
            );
            if let Ok(v) = HeaderValue::from_str(&cookie) {
                out.insert(hyper::header::SET_COOKIE, v);
            }
        }
    }
    Ok(response)
}

async fn handle_ws(req: Request<Incoming>, cfg: Arc<ProxyConfig>) -> Result<Response<Full<Bytes>>> {
    let key = req
        .headers()
        .get("Sec-WebSocket-Key")
        .context("WS upgrade missing Sec-WebSocket-Key")?
        .clone();
    let accept = derive_accept_key(key.as_bytes());
    let subprotocol = req.headers().get("Sec-WebSocket-Protocol").cloned();

    let path = req
        .uri()
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/")
        .to_string();

    let upstream = format!("wss://{}{}", cfg.upstream.host(), path);
    let mut up_req = upstream
        .clone()
        .into_client_request()
        .with_context(|| format!("building upstream WS request for {upstream}"))?;
    {
        let h = up_req.headers_mut();
        h.insert(
            HeaderName::from_static("x-aws-proxy-auth"),
            HeaderValue::from_str(&cfg.upstream.token()).context("auth header")?,
        );
        h.insert(
            HeaderName::from_static("x-aws-proxy-port"),
            HeaderValue::from_str(&cfg.port_for(&path).to_string()).context("port header")?,
        );
        if let Some(sp) = &subprotocol {
            h.insert("Sec-WebSocket-Protocol", sp.clone());
        }
    }

    let (upstream_ws, _resp) = tokio_tungstenite::connect_async(up_req)
        .await
        .with_context(|| format!("connecting upstream WSS {upstream}"))?;

    // Count the pump as one live session.
    let on_upgrade = hyper::upgrade::on(req);
    let activity = cfg.activity.clone();
    if let Some(a) = &activity {
        a.enter();
    }
    tokio::spawn(async move {
        match on_upgrade.await {
            Ok(upgraded) => {
                let browser_ws = tokio_tungstenite::WebSocketStream::from_raw_socket(
                    TokioIo::new(upgraded),
                    Role::Server,
                    None,
                )
                .await;
                pump(browser_ws, upstream_ws).await;
            }
            Err(e) => tracing::warn!(target: "hellbox::proxy", "browser upgrade failed: {e:#}"),
        }
        if let Some(a) = &activity {
            a.leave();
        }
    });

    let mut resp = Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header(
            "Sec-WebSocket-Accept",
            HeaderValue::from_str(&accept).context("accept header")?,
        );
    if let Some(sp) = subprotocol {
        resp = resp.header("Sec-WebSocket-Protocol", sp);
    }
    resp.body(Full::new(Bytes::new()))
        .context("building 101 response")
}

async fn pump<B, U>(browser: B, upstream: U)
where
    B: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>>
        + SinkExt<Message, Error = tokio_tungstenite::tungstenite::Error>
        + Unpin,
    U: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>>
        + SinkExt<Message, Error = tokio_tungstenite::tungstenite::Error>
        + Unpin,
{
    let (mut b_tx, mut b_rx) = browser.split();
    let (mut u_tx, mut u_rx) = upstream.split();

    let b2u = async {
        while let Some(msg) = b_rx.next().await {
            match msg {
                Ok(m) => {
                    if u_tx.send(m).await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = u_tx.close().await;
    };
    let u2b = async {
        while let Some(msg) = u_rx.next().await {
            match msg {
                Ok(m) => {
                    if b_tx.send(m).await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = b_tx.close().await;
    };

    tokio::select! {
        _ = b2u => {}
        _ = u2b => {}
    }
    tracing::debug!(target: "hellbox::proxy", "WS session closed");
}

fn is_websocket_upgrade(headers: &HeaderMap) -> bool {
    let has_upgrade = headers
        .get(hyper::header::CONNECTION)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_ascii_lowercase().contains("upgrade"))
        .unwrap_or(false);
    let is_ws = headers
        .get(hyper::header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("websocket"))
        .unwrap_or(false);
    has_upgrade && is_ws
}

fn build_upstream_headers(
    inbound: &HeaderMap,
    auth_token: &str,
    port: i32,
) -> reqwest::header::HeaderMap {
    let mut out = reqwest::header::HeaderMap::new();
    for (name, value) in inbound.iter() {
        let n = name.as_str();
        if is_hop_by_hop(n) || n.eq_ignore_ascii_case("host") {
            continue;
        }
        if n.eq_ignore_ascii_case("cookie") {
            if let Ok(cookie) = value.to_str()
                && let Some(filtered) = strip_control_cookie(cookie)
                && let Ok(hv) = reqwest::header::HeaderValue::from_str(&filtered)
            {
                out.append(reqwest::header::COOKIE, hv);
            }
            continue;
        }
        if let (Ok(hn), Ok(hv)) = (
            reqwest::header::HeaderName::from_bytes(name.as_str().as_bytes()),
            reqwest::header::HeaderValue::from_bytes(value.as_bytes()),
        ) {
            out.append(hn, hv);
        }
    }
    if let Ok(v) = reqwest::header::HeaderValue::from_str(auth_token) {
        out.insert(
            reqwest::header::HeaderName::from_static("x-aws-proxy-auth"),
            v,
        );
    }
    if let Ok(v) = reqwest::header::HeaderValue::from_str(&port.to_string()) {
        out.insert(
            reqwest::header::HeaderName::from_static("x-aws-proxy-port"),
            v,
        );
    }
    out
}

fn is_hop_by_hop(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

/// Accept only loopback Host/Origin authority.
fn is_loopback_authority(value: &str) -> bool {
    let v = value.trim();
    let v = v
        .strip_prefix("http://")
        .or_else(|| v.strip_prefix("https://"))
        .unwrap_or(v);
    let v = v.split('/').next().unwrap_or(v);
    let host = if let Some(rest) = v.strip_prefix('[') {
        rest.split(']').next().unwrap_or(rest)
    } else {
        v.rsplit_once(':').map(|(h, _)| h).unwrap_or(v)
    };
    matches!(host, "127.0.0.1" | "localhost" | "::1")
}

fn loopback_metadata_ok(headers: &HeaderMap) -> bool {
    let host_ok = headers
        .get(hyper::header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(is_loopback_authority)
        .unwrap_or(false);
    let origin_ok = match headers
        .get(hyper::header::ORIGIN)
        .and_then(|v| v.to_str().ok())
    {
        Some(o) => is_loopback_authority(o),
        None => true,
    };
    host_ok && origin_ok
}

fn has_local_session(headers: &HeaderMap, secret: &str) -> bool {
    headers
        .get(hyper::header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(|cookie| cookie_has_control_secret(cookie, secret))
        .unwrap_or(false)
}

fn allowed_initial_navigation(req: &Request<Incoming>) -> bool {
    if req.method() != hyper::Method::GET || req.uri().path() != "/" {
        return false;
    }
    match req
        .headers()
        .get("sec-fetch-site")
        .and_then(|v| v.to_str().ok())
    {
        Some("same-origin" | "none") | None => true,
        Some(_) => false,
    }
}

fn expected_forward_path(path: &str) -> bool {
    path == "/"
        || path == "/vnc.html"
        || path == "/websockify"
        || path == "/favicon.ico"
        || path.starts_with("/hellbox/audio")
        || path.starts_with("/hellbox/video")
        || path.starts_with("/hellbox/input")
        || path.starts_with("/ldoom/audio")
        || path.starts_with("/ldoom/video")
        || path.starts_with("/ldoom/input")
        || path.starts_with("/app/")
        || path.starts_with("/core/")
        || path.starts_with("/vendor/")
        || path.starts_with("/include/")
        || path.starts_with("/images/")
        || path.starts_with("/utils/")
}

fn data_plane_rejection(
    req: &Request<Incoming>,
    cfg: &ProxyConfig,
) -> Option<Response<Full<Bytes>>> {
    let ctrl = cfg.control.as_ref()?;
    if !expected_forward_path(req.uri().path()) {
        return Some(json_response(
            StatusCode::FORBIDDEN,
            r#"{"error":"unexpected local proxy path"}"#.to_string(),
        ));
    }
    if !loopback_metadata_ok(req.headers()) {
        return Some(json_response(
            StatusCode::FORBIDDEN,
            r#"{"error":"data-plane proxy is loopback-only"}"#.to_string(),
        ));
    }
    if allowed_initial_navigation(req) || has_local_session(req.headers(), &ctrl.control_secret) {
        return None;
    }
    Some(json_response(
        StatusCode::FORBIDDEN,
        r#"{"error":"missing local session secret"}"#.to_string(),
    ))
}

fn strip_control_cookie(cookie: &str) -> Option<String> {
    let kept: Vec<&str> = cookie
        .split(';')
        .map(str::trim)
        .filter(|part| {
            !part
                .strip_prefix(CONTROL_COOKIE_NAME)
                .map(|rest| rest.starts_with('='))
                .unwrap_or(false)
                && !part.is_empty()
        })
        .collect();
    if kept.is_empty() {
        None
    } else {
        Some(kept.join("; "))
    }
}

use tokio_tungstenite::tungstenite::client::IntoClientRequest;

// Browser control plane: state, suspend, resume, injected UI.

fn html_response(body: String) -> Response<Full<Bytes>> {
    let mut resp = Response::new(Full::new(Bytes::from(body)));
    resp.headers_mut().insert(
        hyper::header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    resp
}

fn html_response_with_control_secret(body: String, secret: &str) -> Response<Full<Bytes>> {
    let mut resp = html_response(body);
    let cookie =
        format!("hellbox_control={secret}; Path=/; HttpOnly; SameSite=Strict; Max-Age=86400");
    if let Ok(v) = HeaderValue::from_str(&cookie) {
        resp.headers_mut().insert(hyper::header::SET_COOKIE, v);
    }
    resp
}

fn json_response(status: StatusCode, body: String) -> Response<Full<Bytes>> {
    let mut resp = Response::new(Full::new(Bytes::from(body)));
    *resp.status_mut() = status;
    resp.headers_mut().insert(
        hyper::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    resp
}

async fn handle_control(
    req: Request<Incoming>,
    cfg: Arc<ProxyConfig>,
) -> Result<Response<Full<Bytes>>> {
    let ctrl = match &cfg.control {
        Some(c) => c.clone(),
        None => {
            return Ok(json_response(
                StatusCode::NOT_FOUND,
                r#"{"error":"control disabled"}"#.to_string(),
            ));
        }
    };
    // Local control calls use the user's AWS creds; require loopback metadata.
    if !loopback_metadata_ok(req.headers()) {
        tracing::warn!(target: "hellbox::proxy", "rejected control request (non-loopback Host or Origin)");
        return Ok(json_response(
            StatusCode::FORBIDDEN,
            r#"{"error":"control endpoints are loopback-only"}"#.to_string(),
        ));
    }

    if !has_local_session(req.headers(), &ctrl.control_secret) {
        tracing::warn!(target: "hellbox::proxy", "rejected control request (missing/invalid local session secret)");
        return Ok(json_response(
            StatusCode::FORBIDDEN,
            r#"{"error":"missing local session secret"}"#.to_string(),
        ));
    }

    let method = req.method().clone();
    let action = control_action(req.uri().path()).unwrap_or_default();

    // Keep mutating actions off simple GETs.
    if matches!(action.as_str(), "suspend" | "resume") && method != hyper::Method::POST {
        return Ok(json_response(
            StatusCode::METHOD_NOT_ALLOWED,
            r#"{"error":"use POST"}"#.to_string(),
        ));
    }

    let result: Result<String> = match action.as_str() {
        "state" => current_state(&ctrl).await,
        "suspend" => do_suspend(&ctrl).await,
        "resume" => do_resume(&ctrl).await,
        _ => {
            return Ok(json_response(
                StatusCode::NOT_FOUND,
                r#"{"error":"unknown control action"}"#.to_string(),
            ));
        }
    };

    Ok(match result {
        Ok(state) => json_response(
            StatusCode::OK,
            format!(r#"{{"state":{}}}"#, json_str(&state)),
        ),
        Err(e) => {
            tracing::warn!(target: "hellbox::proxy", "control {action} failed: {e:#}");
            json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(r#"{{"error":{}}}"#, json_str(&format!("{e:#}"))),
            )
        }
    })
}

async fn current_state(ctrl: &ProxyControl) -> Result<String> {
    microvm_state(&ctrl.microvm, &ctrl.microvm_id).await
}

async fn do_suspend(ctrl: &ProxyControl) -> Result<String> {
    tracing::info!(target: "hellbox::proxy", "browser requested suspend of {}", ctrl.microvm_id);
    ctrl.microvm
        .suspend_microvm()
        .microvm_identifier(&ctrl.microvm_id)
        .send()
        .await
        .context("suspend_microvm")?;
    let state = poll_state(ctrl, &["SUSPENDED", "TERMINATED", "FAILED"]).await?;
    record_state(&ctrl.name, &state);
    Ok(state)
}

async fn do_resume(ctrl: &ProxyControl) -> Result<String> {
    tracing::info!(target: "hellbox::proxy", "browser requested resume of {}", ctrl.microvm_id);
    ctrl.microvm
        .resume_microvm()
        .microvm_identifier(&ctrl.microvm_id)
        .send()
        .await
        .context("resume_microvm")?;
    let state = poll_state(ctrl, &["RUNNING", "TERMINATED", "FAILED"]).await?;
    if state == "RUNNING" {
        let host = host_of(&microvm_endpoint(&ctrl.microvm, &ctrl.microvm_id).await?);
        let token = mint_token(ctrl).await?;
        ctrl.upstream.set(host.clone(), token);
        record_endpoint(&ctrl.name, &state, &host);
        tracing::info!(target: "hellbox::proxy", "resumed {} — endpoint+token refreshed", ctrl.microvm_id);
    } else {
        record_state(&ctrl.name, &state);
    }
    Ok(state)
}

async fn mint_token(ctrl: &ProxyControl) -> Result<String> {
    let mut req = ctrl
        .microvm
        .create_microvm_auth_token()
        .microvm_identifier(&ctrl.microvm_id)
        .expiration_in_minutes(TOKEN_TTL_MINUTES);
    for p in &ctrl.token_ports {
        req = req.allowed_ports(PortSpecification::Port(*p));
    }
    let out = req.send().await.context("create_microvm_auth_token")?;
    out.auth_token()
        .get(AUTH_TOKEN_KEY)
        .cloned()
        .with_context(|| format!("auth token response missing '{AUTH_TOKEN_KEY}'"))
}

async fn poll_state(ctrl: &ProxyControl, terminal: &[&str]) -> Result<String> {
    let opts = PollOpts {
        interval: std::time::Duration::from_secs(2),
        timeout: std::time::Duration::from_secs(180),
    };
    let label = format!("microvm {}", ctrl.name);
    poll_microvm_state(&ctrl.microvm, &label, &ctrl.microvm_id, terminal, opts).await
}

fn record_state(name: &str, state: &str) {
    if let Ok(mut st) = State::load() {
        let _ = st.upsert(name, |c| c.state = Some(state.to_string()));
    }
}

fn record_endpoint(name: &str, state: &str, host: &str) {
    if let Ok(mut st) = State::load() {
        let _ = st.upsert(name, |c| {
            c.state = Some(state.to_string());
            c.endpoint = Some(host.to_string());
        });
    }
}

fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn control_action(path: &str) -> Option<String> {
    path.strip_prefix("/__hellbox/").map(str::to_string)
}

fn cookie_has_control_secret(cookie: &str, secret: &str) -> bool {
    cookie.split(';').any(|part| {
        let part = part.trim();
        part.strip_prefix(CONTROL_COOKIE_PREFIX)
            .map(|v| v == secret)
            .unwrap_or(false)
    })
}

fn inject_panel(body: &Bytes) -> Bytes {
    match std::str::from_utf8(body) {
        Ok(html) => {
            let injected = if let Some(idx) = html.rfind("</body>") {
                let mut s = String::with_capacity(html.len() + CONTROL_PANEL.len());
                s.push_str(&html[..idx]);
                s.push_str(CONTROL_PANEL);
                s.push_str(&html[idx..]);
                s
            } else {
                format!("{html}{CONTROL_PANEL}")
            };
            Bytes::from(injected)
        }
        Err(_) => body.clone(),
    }
}

fn control_only_page() -> String {
    CONTROL_ONLY_PAGE.to_string()
}

const CONTROL_ONLY_PAGE: &str = r##"<!doctype html><html><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1,viewport-fit=cover">
<title>Hellbox — paused</title>
<style>
:root{--bg:#0A0B0D;--text:#ECE8DF;--muted:#8B919B;--muted2:#7E848E;
  --ember:#FF6B1A;--ember2:#FF8A3D;--green:#57C77E;--amber:#FFB020;--hairline:#23272E;
  --font-ui:-apple-system,BlinkMacSystemFont,"Segoe UI",system-ui,Roboto,sans-serif;
  --font-mono:ui-monospace,"SF Mono","JetBrains Mono",Menlo,Consolas,monospace}
*{box-sizing:border-box}
html,body{margin:0;height:100%}
body{display:flex;flex-direction:column;align-items:center;justify-content:center;gap:20px;
  background:radial-gradient(80% 60% at 50% 42%,rgba(255,107,26,.06) 0%,rgba(0,0,0,0) 60%),var(--bg);
  color:var(--text);font-family:var(--font-ui)}
.brand{position:fixed;top:20px;left:24px;display:flex;align-items:center;gap:12px}
.mark{display:flex;align-items:center;justify-content:center;width:34px;height:34px;flex-shrink:0}
.word{display:flex;align-items:baseline;gap:1px;font-size:16px;letter-spacing:.01em}
.word .dim{font-weight:600;color:#9CA1AB}.word .strong{font-weight:900;color:var(--text)}
.badge{display:flex;align-items:center;justify-content:center;width:64px;height:64px;border-radius:16px;
  background:#101216;border:1px solid var(--hairline);box-shadow:0 12px 40px rgba(0,0,0,.5)}
.chip{display:flex;align-items:center;gap:9px;height:30px;padding:0 13px;border-radius:8px;
  background:#0E1014;border:1px solid #20242B}
#dot{width:8px;height:8px;border-radius:50%;background:var(--amber)}
#status{font-family:var(--font-mono);font-weight:500;font-size:12px;color:#9AA0AA}
#head{margin:0;font-weight:700;font-size:26px;letter-spacing:-.01em}
#sub{margin:0;max-width:440px;text-align:center;color:var(--muted);font-size:14px;line-height:20px}
#btn{display:flex;align-items:center;gap:9px;height:46px;padding:0 24px;border:0;border-radius:11px;
  font-family:var(--font-ui);font-weight:700;font-size:15px;letter-spacing:.01em;color:#1A0E06;cursor:pointer;
  background:linear-gradient(180deg,var(--ember2),var(--ember));
  box-shadow:0 0 0 1px rgba(255,138,61,.4),0 10px 28px rgba(255,107,26,.36)}
#btn:disabled{opacity:.55;cursor:default;box-shadow:none}
.note{display:flex;align-items:center;gap:8px;font-family:var(--font-mono);font-size:12px;color:var(--muted2)}
</style></head>
<body>
  <div class="brand"><div class="mark"><svg width="32" height="32" viewBox="0 0 40 40" fill="none" xmlns="http://www.w3.org/2000/svg"><defs><linearGradient id="hbf" x1="20" y1="3" x2="20" y2="26" gradientUnits="userSpaceOnUse"><stop stop-color="#FFD23D"/><stop offset=".45" stop-color="#FF8A3D"/><stop offset="1" stop-color="#FF6B1A"/></linearGradient></defs><path d="M20 4 C23.5 9.5 21.5 13 23 16 C25 14.5 25 11.5 25 11.5 C27.5 15 27.5 19.5 24.5 22 C22.8 23.4 21.2 24 20 24 C18.8 24 17.2 23.4 15.5 22 C12.5 19.5 13 15 15.5 12.5 C15.5 15 16.5 15.5 17.5 14.5 C16.8 10.5 17 7.5 20 4 Z" fill="url(#hbf)"/><path d="M13 23 C11 20 12.5 17.5 12 15.5 C13.8 16.8 13.8 18.5 13.8 18.5 C15 16 14 14 15.5 12.5 C15 16.5 16.5 18 15.5 21 Z" fill="url(#hbf)" opacity=".85"/><path d="M27 23 C29 20.5 27.8 18 28.2 16 C26.6 17.2 26.6 18.8 26.6 18.8 C25.6 16.5 26.4 14.5 25 13 C25.4 16.6 24 18 25 21 Z" fill="url(#hbf)" opacity=".85"/><path d="M7.5 24.5 H32.5 L30 36 H10 Z" fill="#1B1E24"/><rect x="6" y="22.5" width="28" height="3.4" rx="1.5" fill="#2A2F37"/><path d="M14 26.5 L13 34 M20 26.5 V34 M26 26.5 L27 34" stroke="#FF6B1A" stroke-width="1.2" opacity=".45" stroke-linecap="round"/><circle cx="13.5" cy="37.5" r="1.7" fill="#2A2F37"/><circle cx="26.5" cy="37.5" r="1.7" fill="#2A2F37"/></svg></div><div class="word"><span class="dim">HELL</span><span class="strong">BOX</span></div></div>
  <div class="badge"><svg width="26" height="26" viewBox="0 0 24 24" fill="#FF6B1A"><rect x="6" y="5" width="4" height="14" rx="1.2"/><rect x="14" y="5" width="4" height="14" rx="1.2"/></svg></div>
  <div class="chip"><span id="dot"></span><b id="status">Checking&#8230;</b></div>
  <h2 id="head">Session suspended</h2>
  <p id="sub">Your microVM is frozen and compute billing has stopped.</p>
  <button id="btn" disabled>&#8230;</button>
  <div class="note"><svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="#7E848E" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 7v5l3 2"/><circle cx="12" cy="12" r="9"/></svg>Back on the exact frame in ~2.6s</div>
<script>
var dot=document.getElementById('dot'),status=document.getElementById('status'),
    head=document.getElementById('head'),sub=document.getElementById('sub'),
    btn=document.getElementById('btn'),busy=false,cur='';
function paint(s){
  cur=s;
  if(s==='RUNNING'){dot.style.background='#57C77E';status.textContent='running';
    head.textContent='Session running';
    sub.textContent='The stream should be live. Reload the tab if it does not appear.';
    btn.textContent='Suspend session';btn.disabled=busy;}
  else if(s==='SUSPENDED'){dot.style.background='#FFB020';
    status.textContent='suspended · billing paused';head.textContent='Session suspended';
    sub.textContent='Your microVM is frozen and compute billing has stopped.';
    btn.textContent='Resume game';btn.disabled=busy;}
  else{dot.style.background='#888';status.textContent=s||'…';btn.textContent='…';btn.disabled=true;}
}
function poll(){if(busy)return;
  fetch('/__hellbox/state').then(function(r){return r.json();})
    .then(function(j){if(j.state)paint(j.state);})
    .catch(function(){status.textContent='proxy offline';dot.style.background='#888';});}
btn.onclick=function(){
  if(busy)return;
  var act=cur==='RUNNING'?'suspend':cur==='SUSPENDED'?'resume':null;if(!act)return;
  busy=true;btn.disabled=true;dot.style.background='#58a6ff';
  status.textContent=act==='suspend'?'suspending…':'resuming…';
  fetch('/__hellbox/'+act,{method:'POST'}).then(function(r){return r.json();})
    .then(function(j){busy=false;
      if(act==='resume'&&j.state==='RUNNING'){status.textContent='resumed · loading…';
        setTimeout(function(){var u=new URL(location.href);u.searchParams.set('resumed','1');location.href=u.toString();},700);return;}
      if(j.state)paint(j.state);else status.textContent=j.error||'error';})
    .catch(function(){busy=false;status.textContent='error';});
};
poll();setInterval(poll,3000);
</script></body></html>"##;

/// Injected Suspend/Resume panel.
const CONTROL_PANEL: &str = r##"
<div id="hellbox-ctl" style="position:fixed;bottom:16px;right:16px;z-index:2147483647;
  font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',system-ui,sans-serif;color:#ECE8DF;
  background:rgba(9,10,13,.74);border:1px solid #1F232A;border-radius:12px;padding:8px 8px 8px 14px;
  display:flex;gap:12px;align-items:center;box-shadow:0 12px 34px rgba(0,0,0,.5)">
  <span id="hellbox-dot" style="width:8px;height:8px;border-radius:50%;flex-shrink:0;
    background:#888;display:inline-block"></span>
  <span id="hellbox-status" style="font-size:13px;font-weight:600;letter-spacing:.01em;white-space:nowrap">…</span>
  <button id="hellbox-btn" style="font-family:inherit;font-size:13px;font-weight:600;
    padding:0 14px;height:34px;cursor:pointer;border-radius:8px;border:1px solid #2C313A;
    background:#15181D;color:#ECE8DF;white-space:nowrap" disabled>…</button>
</div>
<script>
(function(){
  var dot=document.getElementById('hellbox-dot');
  var st=document.getElementById('hellbox-status');
  var btn=document.getElementById('hellbox-btn');
  var busy=false, cur='';
  function paint(state){
    cur=state;
    var map={RUNNING:['#57C77E','Running','Suspend'],
             SUSPENDED:['#FFB020','Suspended','Resume']};
    var m=map[state];
    if(m){dot.style.background=m[0];dot.style.boxShadow='0 0 7px '+m[0];
      st.textContent=m[1];btn.textContent=m[2];btn.disabled=busy;}
    else{dot.style.background='#888';dot.style.boxShadow='none';st.textContent=state||'…';btn.textContent='…';btn.disabled=true;}
  }
  function poll(){
    if(busy)return;
    fetch('/__hellbox/state').then(function(r){return r.json();})
      .then(function(j){if(j.state)paint(j.state);}).catch(function(){});
  }
  btn.onclick=function(){
    if(busy)return;
    var act=cur==='RUNNING'?'suspend':cur==='SUSPENDED'?'resume':null;
    if(!act)return;
    busy=true;btn.disabled=true;dot.style.background='#58a6ff';
    st.textContent=act==='suspend'?'Suspending…':'Resuming…';
    fetch('/__hellbox/'+act,{method:'POST'}).then(function(r){return r.json();})
      .then(function(j){
        busy=false;
        if(act==='resume'&&j.state==='RUNNING'){
          st.textContent='Reconnecting…';
          setTimeout(function(){var u=new URL(location.href);u.searchParams.set('resumed','1');location.href=u.toString();},600);return;
        }
        if(j.state)paint(j.state);else st.textContent=j.error||'error';
      })
      .catch(function(){busy=false;st.textContent='error';poll();});
  };
  poll();setInterval(poll,4000);
})();
</script>
"##;

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> ProxyConfig {
        ProxyConfig {
            upstream: Upstream::new(
                "abc.lambda-microvm.us-east-2.on.aws".into(),
                "the.secret.jwe".into(),
            ),
            upstream_port: 6901,
            local_port: 0,
            routes: vec![
                ("/hellbox/audio".into(), 6902),
                ("/hellbox/video".into(), 6903),
                ("/hellbox/input".into(), 6904),
                ("/ldoom/audio".into(), 6902),
                ("/ldoom/video".into(), 6903),
                ("/ldoom/input".into(), 6904),
            ],
            activity: None,
            control: None,
        }
    }

    #[test]
    fn inject_panel_splices_before_body() {
        let html = Bytes::from("<html><body><h1>hi</h1></body></html>");
        let out = inject_panel(&html);
        let s = std::str::from_utf8(&out).unwrap();
        assert!(s.contains("hellbox-ctl"), "panel markup injected");
        let panel_at = s.find("hellbox-ctl").unwrap();
        let body_at = s.find("</body>").unwrap();
        let h1_at = s.find("<h1>hi</h1>").unwrap();
        assert!(
            h1_at < panel_at && panel_at < body_at,
            "panel between content and </body>"
        );
    }

    #[test]
    fn inject_panel_appends_when_no_body_tag() {
        let html = Bytes::from("<div>no body tag</div>");
        let out = inject_panel(&html);
        let s = std::str::from_utf8(&out).unwrap();
        assert!(
            s.starts_with("<div>no body tag</div>"),
            "original content kept"
        );
        assert!(s.contains("hellbox-ctl"), "panel appended when no </body>");
    }

    #[test]
    fn injected_panel_drives_control_endpoints() {
        assert!(CONTROL_PANEL.contains("/__hellbox/state"), "polls state");
        assert!(CONTROL_PANEL.contains("method:'POST'"), "POSTs the action");
        assert!(CONTROL_PANEL.contains("'Suspend'"), "offers Suspend");
        assert!(CONTROL_PANEL.contains("'Resume'"), "offers Resume");
    }

    #[test]
    fn control_only_page_offers_resume() {
        let page = control_only_page();
        assert!(page.contains("Resume game"), "has a Resume control");
        assert!(page.contains("/__hellbox/state"), "polls live state");
        assert!(
            page.contains("resumed"),
            "reconnects after resume (reloads with ?resumed=1)"
        );
    }

    #[test]
    fn json_str_escapes_special_chars() {
        assert_eq!(json_str("RUNNING"), "\"RUNNING\"");
        assert_eq!(json_str("a\"b\\c"), "\"a\\\"b\\\\c\"");
        assert_eq!(json_str("line\nbreak"), "\"line\\nbreak\"");
    }

    #[test]
    fn control_paths_accept_hellbox_namespace() {
        assert_eq!(control_action("/__hellbox/state").unwrap(), "state");
        assert_eq!(control_action("/__hellbox/suspend").unwrap(), "suspend");
        assert!(control_action("/not-control/state").is_none());
    }

    #[test]
    fn control_secret_cookie_must_match() {
        assert!(cookie_has_control_secret(
            "foo=bar; hellbox_control=abc123; theme=dark",
            "abc123"
        ));
        assert!(!cookie_has_control_secret(
            "hellbox_control=wrong",
            "abc123"
        ));
        assert!(!cookie_has_control_secret("other=abc123", "abc123"));
    }

    #[test]
    fn strips_local_control_cookie_before_forwarding() {
        assert_eq!(
            strip_control_cookie("foo=bar; hellbox_control=abc123; theme=dark").as_deref(),
            Some("foo=bar; theme=dark")
        );
        assert_eq!(strip_control_cookie("hellbox_control=abc123"), None);

        let mut inbound = HeaderMap::new();
        inbound.insert(
            "cookie",
            HeaderValue::from_static("foo=bar; hellbox_control=abc123"),
        );
        let out = build_upstream_headers(&inbound, "the.secret.jwe", 6901);
        assert_eq!(out.get("cookie").unwrap(), "foo=bar");
    }

    #[test]
    fn data_plane_metadata_rejects_foreign_origin() {
        let mut h = HeaderMap::new();
        h.insert("host", HeaderValue::from_static("127.0.0.1:6080"));
        h.insert(
            "origin",
            HeaderValue::from_static("https://foreign.example"),
        );
        assert!(!loopback_metadata_ok(&h));

        h.insert("origin", HeaderValue::from_static("http://127.0.0.1:6080"));
        assert!(loopback_metadata_ok(&h));
    }

    #[test]
    fn forwarded_paths_are_limited_to_stream_and_novnc_assets() {
        assert!(expected_forward_path("/"));
        assert!(expected_forward_path("/hellbox/video"));
        assert!(expected_forward_path("/hellbox/input/ev"));
        assert!(expected_forward_path("/ldoom/video"));
        assert!(expected_forward_path("/ldoom/input/ev"));
        assert!(expected_forward_path("/websockify"));
        assert!(expected_forward_path("/core/rfb.js"));
        assert!(!expected_forward_path("/__hellbox/state"));
        assert!(!expected_forward_path("/random/admin"));
    }

    #[test]
    fn host_of_strips_scheme_and_slash() {
        assert_eq!(
            host_of("https://x.lambda-microvm.us-east-2.on.aws/"),
            "x.lambda-microvm.us-east-2.on.aws"
        );
        assert_eq!(host_of("wss://h/"), "h");
        assert_eq!(host_of("bare.host"), "bare.host");
    }

    #[test]
    fn routes_audio_path_to_audio_port() {
        let c = cfg();
        assert_eq!(c.port_for("/hellbox/audio"), 6902);
        assert_eq!(c.port_for("/hellbox/audio?x=1"), 6902);
        assert_eq!(c.port_for("/ldoom/audio"), 6902);
        assert_eq!(c.port_for("/ldoom/audio?x=1"), 6902);
        assert_eq!(c.port_for("/"), 6901);
        assert_eq!(c.port_for("/websockify"), 6901);
        assert_eq!(c.port_for("/vnc.html"), 6901);
    }

    #[test]
    fn routes_video_and_input_paths() {
        let c = cfg();
        assert_eq!(c.port_for("/hellbox/video"), 6903);
        assert_eq!(c.port_for("/hellbox/video?x=1"), 6903);
        assert_eq!(c.port_for("/hellbox/input"), 6904);
        assert_eq!(c.port_for("/hellbox/input/ev"), 6904);
        assert_eq!(c.port_for("/ldoom/video"), 6903);
        assert_eq!(c.port_for("/ldoom/video?x=1"), 6903);
        assert_eq!(c.port_for("/ldoom/input"), 6904);
        assert_eq!(c.port_for("/ldoom/input/ev"), 6904);
        assert_eq!(c.port_for("/"), 6901);
    }

    #[test]
    fn injects_auth_and_port_headers() {
        let mut inbound = HeaderMap::new();
        inbound.insert("host", HeaderValue::from_static("127.0.0.1:6080"));
        inbound.insert("user-agent", HeaderValue::from_static("test"));
        let out = build_upstream_headers(&inbound, "the.secret.jwe", 6901);

        assert_eq!(out.get(AUTH_HEADER).unwrap(), "the.secret.jwe");
        assert_eq!(out.get(PORT_HEADER).unwrap(), "6901");
        assert_eq!(out.get("user-agent").unwrap(), "test");
    }

    #[test]
    fn strips_hop_by_hop_and_host() {
        let mut inbound = HeaderMap::new();
        inbound.insert("host", HeaderValue::from_static("127.0.0.1:6080"));
        inbound.insert("connection", HeaderValue::from_static("keep-alive"));
        inbound.insert("keep-alive", HeaderValue::from_static("timeout=5"));
        inbound.insert("upgrade", HeaderValue::from_static("h2c"));
        let out = build_upstream_headers(&inbound, "the.secret.jwe", 6901);

        assert!(out.get("host").is_none(), "host must be dropped");
        assert!(out.get("connection").is_none());
        assert!(out.get("keep-alive").is_none());
        assert!(out.get("upgrade").is_none());
    }

    #[test]
    fn loopback_authority_accepts_local_rejects_foreign() {
        assert!(is_loopback_authority("127.0.0.1:6080"));
        assert!(is_loopback_authority("127.0.0.1"));
        assert!(is_loopback_authority("localhost:6080"));
        assert!(is_loopback_authority("[::1]:6080"));
        assert!(is_loopback_authority("http://127.0.0.1:6080"));
        assert!(is_loopback_authority("http://localhost:6080"));
        assert!(is_loopback_authority("http://[::1]:6080"));
        assert!(!is_loopback_authority("foreign.example"));
        assert!(!is_loopback_authority("http://foreign.example"));
        assert!(!is_loopback_authority("http://foreign.example:6080"));
        assert!(!is_loopback_authority("127.0.0.1.foreign.example"));
        assert!(!is_loopback_authority(
            "http://127.0.0.1.foreign.example:6080"
        ));
    }

    #[test]
    fn hop_by_hop_classification() {
        assert!(is_hop_by_hop("Connection"));
        assert!(is_hop_by_hop("Transfer-Encoding"));
        assert!(is_hop_by_hop("upgrade"));
        assert!(!is_hop_by_hop("content-type"));
        assert!(!is_hop_by_hop("x-aws-proxy-auth"));
    }

    #[test]
    fn detects_websocket_upgrade() {
        let mut h = HeaderMap::new();
        h.insert("connection", HeaderValue::from_static("Upgrade"));
        h.insert("upgrade", HeaderValue::from_static("websocket"));
        assert!(is_websocket_upgrade(&h));

        let mut plain = HeaderMap::new();
        plain.insert("connection", HeaderValue::from_static("keep-alive"));
        assert!(!is_websocket_upgrade(&plain));
    }
}
