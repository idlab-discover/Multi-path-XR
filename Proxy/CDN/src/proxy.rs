use crate::{
    config::{CacheMode, Config, KeyNormalizationMode},
    error::AppError,
    http_util::strip_hop_by_hop_headers,
    stats::{CacheBytesTier, Stats},
    storage::{
        CacheKey, CacheKind, CacheManager, Inflight, InflightHead, InflightKey, InflightMethod,
    },
};

use axum::{
    body::Body,
    extract::ws::{Message as AxumWsMessage, WebSocket, WebSocketUpgrade},
    extract::State,
    http::{
        header::{ACCEPT_ENCODING, CACHE_CONTROL, EXPIRES, HOST, PRAGMA},
        HeaderMap, HeaderName, HeaderValue, Method, Response, StatusCode, Uri,
    },
    response::IntoResponse,
    Json,
};
use bytes::Bytes;
use futures_util::{stream, SinkExt, StreamExt, TryStream, TryStreamExt};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use url::Url;

/// Shared application state.
pub struct AppState {
    pub cfg: Config,
    pub origin: OriginClient,
    pub cache: Arc<CacheManager>,
    pub stats: Arc<Stats>,
    origin_base: Url,
}

impl AppState {
    pub fn new(
        cfg: Config,
        origin: OriginClient,
        cache: Arc<CacheManager>,
        stats: Arc<Stats>,
    ) -> Self {
        let origin_base = Url::parse(&cfg.origin_base_url)
            .map_err(|e| AppError::Config(format!("invalid origin_base_url: {e}")))
            .expect("origin_base_url validated before AppState::new");
        Self {
            cfg,
            origin,
            cache,
            stats,
            origin_base,
        }
    }
}

/// Reqwest-based origin client (HTTPS capable).
pub struct OriginClient {
    client: reqwest::Client,
    timeout: Duration,
}

impl OriginClient {
    pub fn new(cfg: &Config) -> Result<Self, AppError> {
        let client = reqwest::Client::builder()
            .tcp_nodelay(true)
            .http2_adaptive_window(true)
            .pool_max_idle_per_host(32)
            .build()
            .map_err(|e| AppError::Config(format!("reqwest build: {e}")))?;

        Ok(Self {
            client,
            timeout: cfg.origin_timeout(),
        })
    }
}

pub async fn stats_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    Json(state.stats.snapshot())
}

pub async fn metrics_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let txt = state.stats.to_prometheus_text();
    (StatusCode::OK, txt)
}

/// Socket.IO (Engine.IO) passthrough handler:
/// - forwards HTTP long-polling (GET/POST/OPTIONS)
/// - relays WebSocket upgrades end-to-end
/// - bypasses caching entirely
pub async fn socketio_handler(
    State(state): State<Arc<AppState>>,
    ws: Option<WebSocketUpgrade>,
    method: Method,
    uri: Uri,
    mut headers: HeaderMap,
    body: Body,
) -> Result<Response<Body>, AppError> {
    // If this is a WS upgrade request, Axum will populate `ws`.
    if let Some(ws) = ws {
        let path_and_query = uri
            .path_and_query()
            .map(|pq| pq.as_str())
            .unwrap_or(uri.path());

        let origin_http = build_origin_url(&state.origin_base, path_and_query)?;
        let origin_ws = to_ws_url(&origin_http)?;

        // Optional: keep cookies for setups that rely on them
        let cookie = headers.get("cookie").cloned();

        return Ok(ws.on_upgrade(move |client_ws| async move {
            if let Err(e) = relay_websocket(client_ws, origin_ws, cookie).await {
                tracing::warn!(error = %e, "socket.io ws relay ended with error");
            }
        }));
    }

    // Otherwise, proxy the HTTP request (Engine.IO polling).
    // Engine.IO will use POST; allow it here.
    match method {
        Method::GET | Method::POST | Method::OPTIONS | Method::HEAD => {}
        _ => return Err(AppError::MethodNotAllowed),
    }

    // For polling we can strip hop-by-hop headers safely.
    strip_hop_by_hop_headers(&mut headers);

    let path_and_query = uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or(uri.path());

    proxy_http_passthrough(state, method, path_and_query, headers, body).await
}

fn to_ws_url(http_url: &Url) -> Result<Url, AppError> {
    let mut u = http_url.clone();
    match u.scheme() {
        "http" => u
            .set_scheme("ws")
            .map_err(|_| AppError::Internal("ws scheme".into()))?,
        "https" => u
            .set_scheme("wss")
            .map_err(|_| AppError::Internal("wss scheme".into()))?,
        _ => {
            return Err(AppError::BadRequest(
                "unsupported origin scheme for ws".into(),
            ))
        }
    }
    Ok(u)
}

async fn proxy_http_passthrough(
    state: Arc<AppState>,
    method: Method,
    path_and_query: &str,
    headers: HeaderMap,
    body: Body,
) -> Result<Response<Body>, AppError> {
    let url = build_origin_url(&state.origin_base, path_and_query)?;
    let mut req = state.origin.client.request(method.clone(), url);

    // Keep it simple and predictable for intermediates/caching layers.
    req = req.header(ACCEPT_ENCODING, "identity");

    for (k, v) in headers.iter() {
        if *k == axum::http::header::HOST || *k == axum::http::header::ACCEPT_ENCODING {
            continue;
        }

        req = req.header(k, v);
    }

    // Stream the incoming request body to the origin without buffering.
    let body_stream = body.into_data_stream();
    let req_body = reqwest::Body::wrap_stream(body_stream);
    req = req.body(req_body);

    state.stats.inc_origin_fetch();

    let resp = tokio::time::timeout(state.origin.timeout, req.send())
        .await
        .map_err(|_| AppError::Origin("origin timeout".into()))?
        .map_err(|e| {
            state.stats.inc_origin_error();
            AppError::Origin(e.to_string())
        })?;

    let status = resp.status();
    let resp_headers = resp.headers().clone();

    if method == Method::HEAD {
        return Ok(build_head_response(
            status,
            resp_headers,
            "socketio",
            std::time::Instant::now(),
        ));
    }

    let stream = resp
        .bytes_stream()
        .map_err(|e| AppError::Origin(e.to_string()));
    let body = Body::from_stream(stream);

    Ok(build_stream_response(
        status,
        resp_headers,
        body,
        "socketio",
        std::time::Instant::now(),
    ))
}

async fn relay_websocket(
    client_ws: WebSocket,
    origin_ws: Url,
    cookie: Option<HeaderValue>,
) -> Result<(), AppError> {
    use tokio_tungstenite::tungstenite;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    // Build an upstream request so we can forward cookies if needed.
    let mut req = origin_ws
        .as_str()
        .into_client_request()
        .map_err(|e| AppError::BadRequest(format!("bad ws url: {e}")))?;
    if let Some(c) = cookie {
        // This works with tungstenite's request type; if cookie forwarding
        // isn’t needed for your setup, you can drop this block.
        req.headers_mut().insert("cookie", c);
    }

    let (upstream_ws, _) = tokio::time::timeout(
        std::time::Duration::from_secs(65),
        tokio_tungstenite::connect_async(req),
    )
    .await
    .map_err(|_| AppError::Origin("ws connect timeout".into()))?
    .map_err(|e| AppError::Origin(format!("ws connect error: {e}")))?;

    let (mut c_tx, mut c_rx) = client_ws.split();
    let (mut u_tx, mut u_rx) = upstream_ws.split();

    let client_to_upstream = async {
        while let Some(msg) = c_rx.next().await {
            let msg = match msg {
                Ok(m) => m,
                Err(_) => break,
            };

            let out = match msg {
                AxumWsMessage::Text(s) => tungstenite::Message::Text(s),
                AxumWsMessage::Binary(b) => tungstenite::Message::Binary(b),
                AxumWsMessage::Ping(p) => tungstenite::Message::Ping(p),
                AxumWsMessage::Pong(p) => tungstenite::Message::Pong(p),
                AxumWsMessage::Close(_) => tungstenite::Message::Close(None),
            };

            if u_tx.send(out).await.is_err() {
                break;
            }
        }
        let _ = u_tx.send(tungstenite::Message::Close(None)).await;
    };

    let upstream_to_client = async {
        while let Some(msg) = u_rx.next().await {
            let msg = match msg {
                Ok(m) => m,
                Err(_) => break,
            };

            let out = match msg {
                tungstenite::Message::Text(s) => AxumWsMessage::Text(s),
                tungstenite::Message::Binary(b) => AxumWsMessage::Binary(b),
                tungstenite::Message::Ping(p) => AxumWsMessage::Ping(p),
                tungstenite::Message::Pong(p) => AxumWsMessage::Pong(p),
                tungstenite::Message::Close(_) => AxumWsMessage::Close(None),
                tungstenite::Message::Frame(_) => continue, // ignore raw frames
            };

            if c_tx.send(out).await.is_err() {
                break;
            }
        }
        let _ = c_tx.send(AxumWsMessage::Close(None)).await;
    };

    tokio::select! {
        _ = client_to_upstream => {},
        _ = upstream_to_client => {},
    }

    Ok(())
}

/// Main proxy handler (fallback route).
pub async fn proxy_handler(
    State(state): State<Arc<AppState>>,
    method: Method,
    uri: Uri,
    mut headers: HeaderMap,
) -> Result<Response<Body>, AppError> {
    let req_start = std::time::Instant::now();
    state.stats.inc_requests();
    state.stats.inc_inflight();
    let stats = state.stats.clone();
    let _inflight_guard = scopeguard::guard((), move |_| stats.dec_inflight());

    if method != Method::GET && method != Method::HEAD {
        return Err(AppError::MethodNotAllowed);
    }

    strip_hop_by_hop_headers(&mut headers);

    let path_and_query = uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or(uri.path());

    let cacheable = is_cacheable_path(&state.cfg, uri.path());

    if request_disables_cache(&headers) {
        return proxy_only(state, method, path_and_query, headers, req_start).await;
    }

    let has_range = headers.get("range").is_some();
    if has_range && !state.cfg.cache_range_requests {
        return proxy_only(state, method, path_and_query, headers, req_start).await;
    }

    if !cacheable {
        // No caching => no per-key lock and no cache-miss stats.
        return proxy_only(state, method, path_and_query, headers, req_start).await;
    }

    let normalized_pq = normalize_path_and_query(&state.cfg, path_and_query);
    let key = compute_key(
        &state.origin_base,
        &normalized_pq,
        &headers,
        state.cfg.cache_range_requests,
    );

    let kind = classify_cache_kind(uri.path());
    let disk_ttl_secs = disk_ttl_secs_for(&state.cfg, kind);
    let inflight_key = inflight_key_for(&method, key);

    // First, try caches without taking any "leader" role.
    if let Some(hit) = state.cache.mem.get(kind, key).await {
        state.stats.inc_hit_mem();
        return Ok(build_mem_response(
            &method,
            hit.value.as_ref(),
            state.stats.clone(),
            req_start,
        ));
    }

    if disk_ttl_secs > 0 {
        if let Some(hit) = state.cache.disk.get(key).await? {
            state.stats.inc_hit_disk();
            let res = build_disk_response(&method, hit, state.stats.clone(), req_start).await?;
            return Ok(res);
        }
    }

    if let Some(inf) = state.cache.get_inflight(inflight_key) {
        return join_inflight(state, method, inf, req_start).await;
    }

    let (inf, is_leader) = state.cache.get_or_create_inflight(inflight_key);
    if !is_leader {
        return join_inflight(state, method, inf, req_start).await;
    }

    state.stats.inc_miss();
    proxy_fetch_and_cache_fanout(
        state,
        key,
        inflight_key,
        method,
        path_and_query,
        headers,
        inf,
        kind,
        disk_ttl_secs,
        req_start,
    )
    .await
}

fn is_cacheable_path(cfg: &Config, path: &str) -> bool {
    match cfg.cache_mode {
        CacheMode::All => true,
        CacheMode::Dash => {
            let p = path.to_ascii_lowercase();
            p.ends_with(".m4s")
                || p.ends_with(".part")
                || p.ends_with(".chunk")
                || p.ends_with(".mp4")
                || p.ends_with(".m4a")
                || p.ends_with(".cmfv")
                || p.ends_with(".cmfa")
                || p.ends_with(".init")
            //|| p.ends_with(".mpd")
        }
    }
}

fn request_disables_cache(headers: &HeaderMap) -> bool {
    header_has_no_cache_value(headers, CACHE_CONTROL)
        || header_has_no_cache_value(headers, PRAGMA)
        || header_expires_immediately(headers)
}

fn header_has_no_cache_value(headers: &HeaderMap, name: HeaderName) -> bool {
    headers
        .get_all(name)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .any(|token| {
            token.eq_ignore_ascii_case("no-cache")
                || token
                    .get(..9)
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case("no-cache="))
        })
}

fn header_expires_immediately(headers: &HeaderMap) -> bool {
    headers
        .get_all(EXPIRES)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .map(str::trim)
        .any(|value| value == "0")
}

fn compute_key(
    origin_base: &Url,
    path_and_query: &str,
    headers: &HeaderMap,
    include_range: bool,
) -> CacheKey {
    let mut h = blake3::Hasher::new();
    h.update(origin_base.as_str().as_bytes());
    h.update(path_and_query.as_bytes());

    if include_range {
        if let Some(v) = headers.get("range") {
            h.update(b"\nrange:");
            h.update(v.as_bytes());
        }
    }

    CacheKey(*h.finalize().as_bytes())
}

fn inflight_key_for(method: &Method, cache_key: CacheKey) -> InflightKey {
    let method = if *method == Method::HEAD {
        InflightMethod::Head
    } else {
        InflightMethod::Get
    };

    InflightKey::new(cache_key, method)
}

fn elapsed_us(req_start: std::time::Instant) -> u64 {
    req_start.elapsed().as_micros().min(u128::from(u64::MAX)) as u64
}

fn record_follower_wait_once(stats: &Stats, waited: &AtomicBool) {
    if !waited.swap(true, Ordering::SeqCst) {
        stats.inc_follower_waits();
    }
}

fn record_immediate_forward(
    stats: &Stats,
    req_start: std::time::Instant,
    payload_bytes: u64,
    cache_tier: Option<CacheBytesTier>,
) {
    if payload_bytes == 0 {
        return;
    }

    stats.set_proxy_first_forward_latency_us(elapsed_us(req_start));
    stats.add_bytes_served(payload_bytes);
    if let Some(cache_tier) = cache_tier {
        stats.add_cache_bytes_served(cache_tier, payload_bytes);
    }
}

fn instrument_forward_stream<S>(
    stream: S,
    stats: Arc<Stats>,
    req_start: std::time::Instant,
    cache_tier: Option<CacheBytesTier>,
    count_origin_bytes: bool,
) -> impl futures_util::Stream<Item = Result<Bytes, AppError>> + Send + 'static
where
    S: TryStream<Ok = Bytes, Error = AppError> + Send + 'static,
{
    let mut first_chunk_forwarded = false;
    stream.map_ok(move |chunk| {
        let chunk_len = chunk.len().min(u64::MAX as usize) as u64;
        if !first_chunk_forwarded {
            first_chunk_forwarded = true;
            stats.set_proxy_first_forward_latency_us(elapsed_us(req_start));
        }
        stats.add_bytes_served(chunk_len);
        if count_origin_bytes {
            stats.add_origin_bytes(chunk_len);
        }
        if let Some(cache_tier) = cache_tier {
            stats.add_cache_bytes_served(cache_tier, chunk_len);
        }
        chunk
    })
}

fn build_mem_response(
    method: &Method,
    v: &crate::storage::memory::MemValue,
    stats: Arc<Stats>,
    req_start: std::time::Instant,
) -> Response<Body> {
    let mut resp = Response::builder().status(v.status);
    {
        let headers = resp.headers_mut().expect("builder headers");
        for (k, val) in &v.headers {
            if let (Ok(name), Ok(value)) = (k.parse::<HeaderName>(), val.parse::<HeaderValue>()) {
                headers.insert(name, value);
            }
        }
        headers.insert("x-cache", "mem".parse().unwrap());
        set_proxy_timing_headers(headers, 0);
    }

    if *method == Method::HEAD {
        resp.body(Body::empty())
            .unwrap_or_else(|_| Response::new(Body::empty()))
    } else {
        record_immediate_forward(
            stats.as_ref(),
            req_start,
            v.body.len().min(u64::MAX as usize) as u64,
            Some(CacheBytesTier::Mem),
        );
        resp.body(Body::from(v.body.clone()))
            .unwrap_or_else(|_| Response::new(Body::empty()))
    }
}

async fn build_disk_response(
    method: &Method,
    hit: crate::storage::DiskHit,
    stats: Arc<Stats>,
    req_start: std::time::Instant,
) -> Result<Response<Body>, AppError> {
    use tokio_util::io::ReaderStream;

    let mut resp = Response::builder().status(hit.status);
    {
        let headers = resp
            .headers_mut()
            .ok_or_else(|| AppError::Internal("header builder".into()))?;
        for (k, v) in &hit.headers {
            if let (Ok(name), Ok(value)) = (k.parse::<HeaderName>(), v.parse::<HeaderValue>()) {
                headers.insert(name, value);
            }
        }
        headers.insert("x-cache", "disk".parse().unwrap());
        set_proxy_timing_headers(headers, 0);
    }

    if *method == Method::HEAD {
        return resp
            .body(Body::empty())
            .map_err(|e| AppError::Internal(format!("{e}")));
    }

    let file = tokio::fs::File::open(&hit.body_path).await?;
    let stream = instrument_forward_stream(
        ReaderStream::new(file).map_err(AppError::Io),
        stats,
        req_start,
        Some(CacheBytesTier::Disk),
        false,
    );
    let body = Body::from_stream(stream);

    resp.body(body)
        .map_err(|e| AppError::Internal(format!("{e}")))
}

async fn proxy_only(
    state: Arc<AppState>,
    method: Method,
    path_and_query: &str,
    headers: HeaderMap,
    req_start: std::time::Instant,
) -> Result<Response<Body>, AppError> {
    let url = build_origin_url(&state.origin_base, path_and_query)?;
    let mut req = state.origin.client.request(method.clone(), url);
    req = forward_headers(req, &headers);

    state.stats.inc_origin_fetch();

    let resp = tokio::time::timeout(state.origin.timeout, req.send())
        .await
        .map_err(|_| AppError::Origin("origin timeout".into()))?
        .map_err(|e| {
            state.stats.inc_origin_error();
            AppError::Origin(e.to_string())
        })?;

    let status = resp.status();
    let resp_headers = resp.headers().clone();

    if method == Method::HEAD {
        return Ok(build_head_response(
            status,
            resp_headers,
            "bypass",
            req_start,
        ));
    }

    let stream = instrument_forward_stream(
        resp.bytes_stream()
            .map_err(|e| AppError::Origin(e.to_string())),
        state.stats.clone(),
        req_start,
        None,
        true,
    );
    let body = Body::from_stream(stream);

    Ok(build_stream_response(
        status,
        resp_headers,
        body,
        "bypass",
        req_start,
    ))
}

fn build_origin_url(origin_base: &Url, path_and_query: &str) -> Result<Url, AppError> {
    let joined = origin_base
        .join(path_and_query)
        .map_err(|e| AppError::BadRequest(format!("bad path: {e}")))?;

    // Hard safety check: never allow escaping the configured origin host/scheme.
    if joined.scheme() != origin_base.scheme()
        || joined.host_str() != origin_base.host_str()
        || joined.port_or_known_default() != origin_base.port_or_known_default()
    {
        return Err(AppError::BadRequest("invalid path".into()));
    }
    Ok(joined)
}

fn forward_headers(
    mut req: reqwest::RequestBuilder,
    headers: &HeaderMap,
) -> reqwest::RequestBuilder {
    // Avoid gzip/br; keep cache keys stable and simplify storage.
    for (k, v) in headers.iter() {
        // Skip headers we explicitly control
        if *k == HOST || *k == ACCEPT_ENCODING {
            continue;
        }

        req = req.header(k, v);
    }

    // Ensure exactly one encoding header
    req.header(ACCEPT_ENCODING, "identity")
}

fn response_headers_to_kv(h: &HeaderMap) -> Vec<(String, String)> {
    // Store only end-to-end headers. We reuse the same stripping logic as for requests.
    let mut filtered = h.clone();
    strip_hop_by_hop_headers(&mut filtered);

    let mut out = Vec::new();
    for (k, v) in filtered.iter() {
        if let Ok(vs) = v.to_str() {
            out.push((k.to_string(), vs.to_string()));
        }
    }
    out
}

fn build_head_response(
    status: StatusCode,
    mut headers: HeaderMap,
    cache_tag: &str,
    req_start: std::time::Instant,
) -> Response<Body> {
    strip_hop_by_hop_headers(&mut headers);
    let mut resp = Response::builder().status(status);
    {
        let backend_wait_ms = req_start.elapsed().as_millis() as u64;
        let hm = resp.headers_mut().unwrap();
        *hm = headers;
        hm.insert("x-cache", cache_tag.parse().unwrap());
        set_proxy_timing_headers(hm, backend_wait_ms);
    }
    resp.body(Body::empty())
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

fn build_stream_response(
    status: StatusCode,
    mut headers: HeaderMap,
    body: Body,
    cache_tag: &str,
    req_start: std::time::Instant,
) -> Response<Body> {
    strip_hop_by_hop_headers(&mut headers);
    let mut resp = Response::builder().status(status);
    {
        let backend_wait_ms = req_start.elapsed().as_millis() as u64;
        let hm = resp.headers_mut().unwrap();
        *hm = headers;
        hm.insert("x-cache", cache_tag.parse().unwrap());
        set_proxy_timing_headers(hm, backend_wait_ms);
    }
    resp.body(body)
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

// Lightweight deferred cleanup helper for short-lived request guards.
mod scopeguard {
    pub fn guard<T, F: FnOnce(T)>(t: T, f: F) -> Guard<T, F> {
        Guard(Some(t), Some(f))
    }
    pub struct Guard<T, F: FnOnce(T)>(Option<T>, Option<F>);
    impl<T, F: FnOnce(T)> Drop for Guard<T, F> {
        fn drop(&mut self) {
            if let (Some(t), Some(f)) = (self.0.take(), self.1.take()) {
                f(t);
            }
        }
    }
}

async fn join_inflight(
    state: Arc<AppState>,
    method: Method,
    inflight: Arc<Inflight>,
    req_start: std::time::Instant,
) -> Result<Response<Body>, AppError> {
    state.stats.inc_coalesced_requests();
    let follower_waited = Arc::new(AtomicBool::new(false));
    let head = wait_inflight_head(&state, &inflight, Some(Arc::clone(&follower_waited))).await?;

    if method == Method::HEAD {
        let backend_wait_ms = req_start.elapsed().as_millis() as u64;
        let mut resp = Response::builder().status(head.status);
        {
            let mut tmp = HeaderMap::new();
            for (k, v) in &head.headers {
                if let (Ok(name), Ok(value)) = (k.parse::<HeaderName>(), v.parse::<HeaderValue>()) {
                    tmp.insert(name, value);
                }
            }
            strip_hop_by_hop_headers(&mut tmp);

            let hm = resp
                .headers_mut()
                .ok_or_else(|| AppError::Internal("header builder".into()))?;
            *hm = tmp;
            hm.insert("x-cache", "inflight".parse().unwrap());
            set_proxy_timing_headers(hm, backend_wait_ms);
        }
        return resp
            .body(Body::empty())
            .map_err(|e| AppError::Internal(format!("{e}")));
    }

    let body_stream = instrument_forward_stream(
        inflight_body_stream(
            inflight.clone(),
            state.stats.clone(),
            Some(Arc::clone(&follower_waited)),
        ),
        state.stats.clone(),
        req_start,
        None,
        false,
    );
    let backend_wait_ms = req_start.elapsed().as_millis() as u64;

    let mut resp = Response::builder().status(head.status);
    {
        let mut tmp = HeaderMap::new();
        for (k, v) in &head.headers {
            if let (Ok(name), Ok(value)) = (k.parse::<HeaderName>(), v.parse::<HeaderValue>()) {
                tmp.insert(name, value);
            }
        }
        strip_hop_by_hop_headers(&mut tmp);

        let hm = resp
            .headers_mut()
            .ok_or_else(|| AppError::Internal("header builder".into()))?;
        *hm = tmp;
        hm.insert("x-cache", "inflight".parse().unwrap());
        set_proxy_timing_headers(hm, backend_wait_ms);
    }

    resp.body(Body::from_stream(body_stream))
        .map_err(|e| AppError::Internal(format!("{e}")))
}

async fn wait_inflight_head(
    state: &Arc<AppState>,
    inflight: &Arc<Inflight>,
    follower_waited: Option<Arc<AtomicBool>>,
) -> Result<InflightHead, AppError> {
    let mut rx = inflight.subscribe_head();

    if let Some(h) = rx.borrow().clone() {
        return Ok(h);
    }

    if let Some(follower_waited) = follower_waited.as_ref() {
        record_follower_wait_once(state.stats.as_ref(), follower_waited.as_ref());
    }

    let fut = async {
        loop {
            rx.changed()
                .await
                .map_err(|_| AppError::Origin("inflight head channel closed".into()))?;
            if let Some(h) = rx.borrow().clone() {
                return Ok(h);
            }
        }
    };

    tokio::time::timeout(state.origin.timeout, fut)
        .await
        .map_err(|_| AppError::Origin("timeout waiting for inflight head".into()))?
}

fn inflight_body_stream(
    inflight: Arc<Inflight>,
    stats: Arc<crate::stats::Stats>,
    follower_waited: Option<Arc<AtomicBool>>,
) -> impl futures_util::Stream<Item = Result<Bytes, AppError>> + Send + 'static {
    #[derive(Debug)]
    struct StreamState {
        inflight: Arc<Inflight>,
        next_index: usize,
        prog_rx: tokio::sync::watch::Receiver<crate::storage::InflightProgress>,
        stats: Arc<crate::stats::Stats>,
        follower_waited: Option<Arc<AtomicBool>>,
    }

    stream::unfold(
        StreamState {
            prog_rx: inflight.subscribe_progress(),
            inflight,
            next_index: 0,
            stats,
            follower_waited,
        },
        |mut st| async move {
            loop {
                match st.inflight.read_at(st.next_index) {
                    crate::storage::InflightRead::Chunk(chunk) => {
                        st.next_index = st.next_index.saturating_add(1);
                        return Some((Ok(chunk), st));
                    }
                    crate::storage::InflightRead::Pending => {
                        if let Some(follower_waited) = st.follower_waited.as_ref() {
                            record_follower_wait_once(st.stats.as_ref(), follower_waited.as_ref());
                        }
                        if st.prog_rx.changed().await.is_err() {
                            return Some((
                                Err(AppError::Origin("inflight progress channel closed".into())),
                                st,
                            ));
                        }
                    }
                    crate::storage::InflightRead::Done => return None,
                    crate::storage::InflightRead::Error(err) => {
                        return Some((Err(AppError::Origin(err.to_string())), st));
                    }
                }
            }
        },
    )
}

#[allow(clippy::too_many_arguments)]
async fn proxy_fetch_and_cache_fanout(
    state: Arc<AppState>,
    key: CacheKey,
    inflight_key: InflightKey,
    method: Method,
    path_and_query: &str,
    headers: HeaderMap,
    inflight: Arc<Inflight>,
    kind: CacheKind,
    disk_ttl_secs: u64,
    req_start: std::time::Instant,
) -> Result<Response<Body>, AppError> {
    let url = build_origin_url(&state.origin_base, path_and_query)?;
    let mut req = state.origin.client.request(method.clone(), url);
    req = forward_headers(req, &headers);

    state.stats.inc_origin_fetch();

    let resp = tokio::time::timeout(state.origin.timeout, req.send())
        .await
        .map_err(|_| AppError::Origin("origin timeout".into()))?
        .map_err(|e| {
            state.stats.inc_origin_error();
            AppError::Origin(e.to_string())
        })?;

    let status = resp.status();
    let resp_headers = resp.headers().clone();
    let headers_kv = response_headers_to_kv(&resp_headers);
    inflight.publish_head(InflightHead {
        status,
        headers: headers_kv.clone(),
    });

    let cache = state.cache.clone();
    let stats = state.stats.clone();
    let cfg = state.cfg.clone();
    let inflight2 = inflight.clone();

    let leader_body = if method == Method::HEAD {
        Body::empty()
    } else {
        Body::from_stream(instrument_forward_stream(
            inflight_body_stream(inflight.clone(), state.stats.clone(), None),
            state.stats.clone(),
            req_start,
            None,
            false,
        ))
    };

    let disk_writer = if method != Method::HEAD && status.is_success() && disk_ttl_secs > 0 {
        let (meta_path, body_path) = cache.disk.paths_for_key(key);
        let (disk_tx, mut disk_rx) = tokio::sync::mpsc::channel::<Bytes>(64);
        let cache2 = cache.clone();
        let headers_kv2 = headers_kv.clone();
        let abort_disk = Arc::new(AtomicBool::new(false));
        let abort_disk_writer = abort_disk.clone();

        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;

            let _ = tokio::fs::remove_file(&meta_path).await;
            let mut out = None;
            let mut created = false;
            let mut keep_disk = true;

            if let Some(parent) = body_path.parent() {
                if tokio::fs::create_dir_all(parent).await.is_ok() {
                    match tokio::fs::File::create(&body_path).await {
                        Ok(file) => {
                            out = Some(file);
                            created = true;
                        }
                        Err(_) => keep_disk = false,
                    }
                } else {
                    keep_disk = false;
                }
            } else {
                keep_disk = false;
            }

            while let Some(chunk) = disk_rx.recv().await {
                if !keep_disk {
                    continue;
                }

                match out.as_mut() {
                    Some(file) => {
                        if file.write_all(&chunk).await.is_err() {
                            keep_disk = false;
                        }
                    }
                    None => keep_disk = false,
                }
            }

            if let Some(mut file) = out {
                if file.flush().await.is_err() {
                    keep_disk = false;
                }
            }

            keep_disk = keep_disk && !abort_disk_writer.load(Ordering::Relaxed);

            if keep_disk && created {
                if let Err(e) = cache2
                    .disk
                    .finalize_meta_atomic_with_ttl(key, status, headers_kv2, disk_ttl_secs)
                    .await
                {
                    tracing::warn!(error = %e, "disk cache finalize failed");
                    let _ = tokio::fs::remove_file(&body_path).await;
                }
            } else if created {
                let _ = tokio::fs::remove_file(&body_path).await;
            }
        });

        Some((disk_tx, abort_disk))
    } else {
        None
    };

    let mut origin_stream = resp.bytes_stream();

    tokio::spawn(async move {
        let mut mem_buf: Vec<u8> = Vec::new();
        let cache_status_ok = status.is_success();
        let mem_ttl_secs = match kind {
            CacheKind::Mpd => cfg.mpd_memory_ttl_secs,
            CacheKind::Segment | CacheKind::Other => cfg.memory_ttl_secs,
        };
        let mem_enabled = cache_status_ok && mem_ttl_secs > 0;
        let mut disk_tx = disk_writer;

        while let Some(item) = origin_stream.next().await {
            match item {
                Ok(chunk) => {
                    stats.add_origin_bytes(chunk.len().min(u64::MAX as usize) as u64);
                    if mem_enabled
                        && (mem_buf.len() as u64).saturating_add(chunk.len() as u64)
                            <= cfg.memory_object_max_bytes
                    {
                        mem_buf.extend_from_slice(&chunk);
                    }

                    inflight2.push_chunk(chunk.clone());

                    if let Some((tx, abort_disk)) = disk_tx.as_ref() {
                        if tx.try_send(chunk).is_err() {
                            abort_disk.store(true, Ordering::Relaxed);
                            disk_tx = None;
                        }
                    }
                }
                Err(e) => {
                    stats.inc_origin_error();
                    inflight2.publish_error(AppError::Origin(e.to_string()));
                    cache.remove_inflight(inflight_key);
                    return;
                }
            }
        }

        drop(disk_tx);

        if mem_enabled && !mem_buf.is_empty() {
            cache
                .mem
                .insert(
                    kind,
                    key,
                    crate::storage::memory::MemValue {
                        status,
                        headers: headers_kv,
                        body: Bytes::from(mem_buf),
                    },
                )
                .await;
        }

        inflight2.publish_done();
        cache.remove_inflight(inflight_key);
    });

    Ok(build_stream_response(
        status,
        resp_headers,
        leader_body,
        "miss",
        req_start,
    ))
}

fn classify_cache_kind(path: &str) -> CacheKind {
    let p = path.to_ascii_lowercase();
    if p.ends_with(".mpd") {
        return CacheKind::Mpd;
    }
    if p.ends_with(".m4s")
        || p.ends_with(".part")
        || p.ends_with(".chunk")
        || p.ends_with(".mp4")
        || p.ends_with(".m4a")
        || p.ends_with(".cmfv")
        || p.ends_with(".cmfa")
        || p.ends_with(".init")
    {
        return CacheKind::Segment;
    }
    CacheKind::Other
}

fn disk_ttl_secs_for(cfg: &Config, kind: CacheKind) -> u64 {
    match kind {
        CacheKind::Mpd => cfg.mpd_disk_ttl_secs,
        CacheKind::Segment | CacheKind::Other => cfg.disk_ttl_secs,
    }
}

fn normalize_path_and_query(cfg: &Config, path_and_query: &str) -> String {
    // Best-effort only: if parsing fails, fall back to original to preserve correctness.
    let (path, query) = match path_and_query.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (path_and_query, None),
    };

    let Some(qs) = query else {
        return path.to_string();
    };

    match cfg.key_normalization_mode {
        KeyNormalizationMode::None => path_and_query.to_string(),
        KeyNormalizationMode::DropAllQuery => path.to_string(),
        KeyNormalizationMode::Whitelist | KeyNormalizationMode::Blacklist => {
            let wl: std::collections::HashSet<String> = cfg
                .key_query_whitelist
                .iter()
                .map(|s| s.trim().to_ascii_lowercase())
                .filter(|s| !s.is_empty())
                .collect();

            let bl: std::collections::HashSet<String> = cfg
                .key_query_blacklist
                .iter()
                .map(|s| s.trim().to_ascii_lowercase())
                .filter(|s| !s.is_empty())
                .collect();

            // Parse pairs, filter, sort, re-encode in canonical order.
            let mut pairs: Vec<(String, String)> = Vec::new();
            for (k, v) in url::form_urlencoded::parse(qs.as_bytes()) {
                let key_lc = k.to_ascii_lowercase();
                let keep = match cfg.key_normalization_mode {
                    KeyNormalizationMode::Whitelist => !wl.is_empty() && wl.contains(&key_lc),
                    KeyNormalizationMode::Blacklist => !bl.contains(&key_lc),
                    _ => true,
                };
                if keep {
                    pairs.push((k.into_owned(), v.into_owned()));
                }
            }

            if pairs.is_empty() {
                return path.to_string();
            }

            pairs.sort_unstable_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

            let mut ser = url::form_urlencoded::Serializer::new(String::new());
            for (k, v) in pairs {
                ser.append_pair(&k, &v);
            }
            let canon = ser.finish();
            format!("{path}?{canon}")
        }
    }
}

fn unix_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Appends the previous hop's effective values to x-backend-chain and then sets
/// the new effective x-backend-* headers for this hop.
///
fn set_proxy_timing_headers(headers: &mut HeaderMap, backend_wait_ms: u64) {
    let now_ms = unix_now_ms();

    // 1) Capture previous effective values (if present) and append them to the chain.
    if let (Some(prev_now), Some(prev_wait)) = (
        headers
            .get("x-backend-now-ms")
            .and_then(|v| v.to_str().ok()),
        headers
            .get("x-backend-wait-ms")
            .and_then(|v| v.to_str().ok()),
    ) {
        // Format: now:wait
        let entry = format!("{prev_now}:{prev_wait}");

        let new_chain = match headers.get("x-backend-chain").and_then(|v| v.to_str().ok()) {
            Some(existing) if !existing.is_empty() => format!("{existing},{entry}"),
            _ => entry,
        };

        if let Ok(v) = HeaderValue::from_str(&new_chain) {
            headers.insert("x-backend-chain", v);
        }
    }

    // 2) Set this hop's effective values (what the client should use).
    headers.insert(
        "x-backend-now-ms",
        HeaderValue::from_str(&now_ms.to_string())
            .unwrap_or_else(|_| HeaderValue::from_static("0")),
    );
    headers.insert(
        "x-backend-wait-ms",
        HeaderValue::from_str(&backend_wait_ms.to_string())
            .unwrap_or_else(|_| HeaderValue::from_static("0")),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::header::{CACHE_CONTROL, EXPIRES, PRAGMA};

    #[test]
    fn follower_waits_are_counted_once_per_coalesced_request() {
        let stats = Stats::new();
        let waited = AtomicBool::new(false);

        record_follower_wait_once(&stats, &waited);
        record_follower_wait_once(&stats, &waited);

        assert_eq!(stats.snapshot().follower_waits_total, 1);
    }

    #[test]
    fn immediate_forward_records_last_latency_and_cache_bytes() {
        let stats = Stats::new();
        let req_start = std::time::Instant::now();

        record_immediate_forward(&stats, req_start, 128, Some(CacheBytesTier::Mem));

        let snapshot = stats.snapshot();
        assert_eq!(snapshot.bytes_served_total, 128);
        assert_eq!(snapshot.cache_bytes_served_mem_total, 128);
        assert!(snapshot.proxy_first_forward_latency_us <= 1_000_000);
    }

    #[test]
    fn inflight_keys_are_method_aware() {
        let key = CacheKey([42; 32]);

        assert_ne!(
            inflight_key_for(&Method::GET, key),
            inflight_key_for(&Method::HEAD, key)
        );
        assert_eq!(
            inflight_key_for(&Method::GET, key),
            inflight_key_for(&Method::GET, key)
        );
    }

    #[test]
    fn request_disables_cache_for_explicit_request_directives() {
        let mut headers = HeaderMap::new();
        headers.insert(CACHE_CONTROL, HeaderValue::from_static("max-age=60, no-cache"));
        assert!(request_disables_cache(&headers));

        headers.clear();
        headers.insert(PRAGMA, HeaderValue::from_static("no-cache"));
        assert!(request_disables_cache(&headers));

        headers.clear();
        headers.insert(EXPIRES, HeaderValue::from_static("0"));
        assert!(request_disables_cache(&headers));

        headers.clear();
        headers.insert(CACHE_CONTROL, HeaderValue::from_static("public, max-age=60"));
        assert!(!request_disables_cache(&headers));
    }
}
