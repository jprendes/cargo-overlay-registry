use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, HeaderMap, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{get, put},
    Router,
};
use bytes::Bytes;
use log::{error, info, warn};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Upstream crates.io URLs
const CRATES_IO_INDEX: &str = "https://index.crates.io";
const CRATES_IO_API: &str = "https://crates.io";

/// Proxy state containing the HTTP client
struct ProxyState {
    client: Client,
    /// The base URL where this proxy is listening (for config.json rewriting)
    proxy_base_url: String,
}

/// Custom config.json that points cargo to our proxy
#[derive(Serialize, Deserialize)]
struct RegistryConfig {
    dl: String,
    api: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "auth-required")]
    auth_required: Option<bool>,
}

#[tokio::main]
async fn main() {
    // Initialize the logger
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let proxy_port = std::env::var("PROXY_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080u16);

    let proxy_base_url = std::env::var("PROXY_BASE_URL")
        .unwrap_or_else(|_| format!("http://localhost:{}", proxy_port));

    info!("Starting cargo registry proxy on port {}", proxy_port);
    info!("Proxy base URL: {}", proxy_base_url);
    info!("Proxying index from: {}", CRATES_IO_INDEX);
    info!("Proxying API from: {}", CRATES_IO_API);

    let state = Arc::new(ProxyState {
        client: Client::builder()
            .user_agent("cargo-proxy-registry/0.1.0")
            .build()
            .expect("Failed to create HTTP client"),
        proxy_base_url: proxy_base_url.clone(),
    });

    let app = Router::new()
        // Index config endpoint
        .route("/config.json", get(handle_config))
        // Index files for 4+ char package names: /{first_two}/{second_two}/{name}
        .route("/{first_two}/{second_two}/{name}", get(handle_index_4plus))
        // API: Search crates
        .route("/api/v1/crates", get(handle_api_search))
        // API: Publish crate
        .route("/api/v1/crates/new", put(handle_api_publish))
        // API: Download crate
        .route(
            "/api/v1/crates/{crate_name}/{version}/download",
            get(handle_api_download),
        )
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", proxy_port))
        .await
        .expect("Failed to bind to port");

    info!("Listening on 0.0.0.0:{}", proxy_port);
    info!("Configure cargo to use: sparse+{}/", proxy_base_url);

    axum::serve(listener, app).await.expect("Server error");
}

/// Returns a modified config.json pointing to our proxy
async fn handle_config(State(state): State<Arc<ProxyState>>) -> impl IntoResponse {
    info!("GET /config.json - Serving proxy configuration");

    let config = RegistryConfig {
        dl: format!("{}/api/v1/crates", state.proxy_base_url),
        api: state.proxy_base_url.clone(),
        auth_required: None,
    };

    info!(
        "  Response: 200 OK - dl={}, api={}",
        config.dl, config.api
    );

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::to_string_pretty(&config).unwrap(),
    )
}

/// Handle index request for 4+ character package names
async fn handle_index_4plus(
    State(state): State<Arc<ProxyState>>,
    Path((first_two, second_two, name)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let path = format!("/{}/{}/{}", first_two, second_two, name);
    proxy_index_request(&state, &path, &headers).await
}

/// Proxy an index request to crates.io sparse index
async fn proxy_index_request(
    state: &ProxyState,
    path: &str,
    headers: &HeaderMap,
) -> Response {
    let url = format!("{}{}", CRATES_IO_INDEX, path);
    info!("GET {} -> {}", path, url);

    let mut request = state.client.get(&url);

    // Forward caching headers
    if let Some(etag) = headers.get(header::IF_NONE_MATCH) {
        if let Ok(value) = etag.to_str() {
            request = request.header(header::IF_NONE_MATCH, value);
            info!("  Forwarding If-None-Match: {}", value);
        }
    }
    if let Some(modified) = headers.get(header::IF_MODIFIED_SINCE) {
        if let Ok(value) = modified.to_str() {
            request = request.header(header::IF_MODIFIED_SINCE, value);
            info!("  Forwarding If-Modified-Since: {}", value);
        }
    }

    // Forward authorization if present
    if let Some(auth) = headers.get(header::AUTHORIZATION) {
        if let Ok(value) = auth.to_str() {
            request = request.header(header::AUTHORIZATION, value);
            info!("  Forwarding Authorization header");
        }
    }

    match request.send().await {
        Ok(response) => {
            let status = response.status();
            info!("  Response: {} {}", status.as_u16(), status.canonical_reason().unwrap_or(""));

            let mut builder = Response::builder().status(StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR));

            // Forward relevant response headers
            if let Some(etag) = response.headers().get(header::ETAG) {
                builder = builder.header(header::ETAG, etag.clone());
                info!("  Response ETag: {:?}", etag);
            }
            if let Some(last_modified) = response.headers().get(header::LAST_MODIFIED) {
                builder = builder.header(header::LAST_MODIFIED, last_modified.clone());
            }
            if let Some(content_type) = response.headers().get(header::CONTENT_TYPE) {
                builder = builder.header(header::CONTENT_TYPE, content_type.clone());
            }

            match response.bytes().await {
                Ok(body) => {
                    if status.is_success() && body.len() < 1000 {
                        info!("  Body: {}", String::from_utf8_lossy(&body));
                    } else if status.is_success() {
                        info!("  Body: {} bytes", body.len());
                    }
                    builder.body(Body::from(body)).unwrap()
                }
                Err(e) => {
                    error!("  Failed to read response body: {}", e);
                    (StatusCode::BAD_GATEWAY, format!("Failed to read upstream response: {}", e)).into_response()
                }
            }
        }
        Err(e) => {
            error!("  Failed to connect to upstream: {}", e);
            (StatusCode::BAD_GATEWAY, format!("Failed to connect to upstream: {}", e)).into_response()
        }
    }
}

/// Handle API search request: GET /api/v1/crates
async fn handle_api_search(
    State(state): State<Arc<ProxyState>>,
    uri: Uri,
    headers: HeaderMap,
) -> impl IntoResponse {
    let query = uri.query().map(|q| format!("?{}", q)).unwrap_or_default();
    let path = format!("/api/v1/crates{}", query);
    let url = format!("{}{}", CRATES_IO_API, path);

    info!("GET /api/v1/crates{} -> {}", query, url);
    proxy_api_request(&state, Method::GET, &url, &headers, None).await
}

/// Handle API publish request: PUT /api/v1/crates/new
async fn handle_api_publish(
    State(state): State<Arc<ProxyState>>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let url = format!("{}/api/v1/crates/new", CRATES_IO_API);

    info!("PUT /api/v1/crates/new -> {} ({} bytes)", url, body.len());
    proxy_api_request(&state, Method::PUT, &url, &headers, Some(body)).await
}

/// Handle API download request: GET /api/v1/crates/{crate}/{version}/download
async fn handle_api_download(
    State(state): State<Arc<ProxyState>>,
    Path((crate_name, version)): Path<(String, String)>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let url = format!(
        "{}/api/v1/crates/{}/{}/download",
        CRATES_IO_API, crate_name, version
    );

    info!(
        "GET /api/v1/crates/{}/{}/download -> {}",
        crate_name, version, url
    );
    proxy_api_request(&state, Method::GET, &url, &headers, None).await
}

/// Generic API proxy function
async fn proxy_api_request(
    state: &ProxyState,
    method: Method,
    url: &str,
    headers: &HeaderMap,
    body: Option<Bytes>,
) -> Response {
    let mut request = match method {
        Method::GET => state.client.get(url),
        Method::PUT => state.client.put(url),
        Method::DELETE => state.client.delete(url),
        Method::POST => state.client.post(url),
        _ => {
            warn!("  Unsupported method: {}", method);
            return (StatusCode::METHOD_NOT_ALLOWED, "Method not allowed").into_response();
        }
    };

    // Forward authorization header
    if let Some(auth) = headers.get(header::AUTHORIZATION) {
        if let Ok(value) = auth.to_str() {
            request = request.header(header::AUTHORIZATION, value);
            info!("  Forwarding Authorization header");
        }
    }

    // Forward content-type
    if let Some(content_type) = headers.get(header::CONTENT_TYPE) {
        if let Ok(value) = content_type.to_str() {
            request = request.header(header::CONTENT_TYPE, value);
        }
    }

    // Forward accept header
    if let Some(accept) = headers.get(header::ACCEPT) {
        if let Ok(value) = accept.to_str() {
            request = request.header(header::ACCEPT, value);
        }
    }

    // Add body if present
    if let Some(body) = body {
        request = request.body(body);
    }

    match request.send().await {
        Ok(response) => {
            let status = response.status();
            info!("  Response: {} {}", status.as_u16(), status.canonical_reason().unwrap_or(""));

            let mut builder = Response::builder().status(StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR));

            // Forward response headers
            for (key, value) in response.headers().iter() {
                if key != header::TRANSFER_ENCODING && key != header::CONNECTION {
                    builder = builder.header(key.clone(), value.clone());
                }
            }

            match response.bytes().await {
                Ok(body) => {
                    // Log response body for JSON responses (but not large binary files)
                    if body.len() < 5000 {
                        if let Ok(text) = std::str::from_utf8(&body) {
                            if text.starts_with('{') || text.starts_with('[') {
                                info!("  Response body: {}", text);
                            }
                        }
                    } else {
                        info!("  Response body: {} bytes (binary/large)", body.len());
                    }
                    builder.body(Body::from(body)).unwrap()
                }
                Err(e) => {
                    error!("  Failed to read response body: {}", e);
                    (StatusCode::BAD_GATEWAY, format!("Failed to read upstream response: {}", e)).into_response()
                }
            }
        }
        Err(e) => {
            error!("  Failed to connect to upstream: {}", e);
            (StatusCode::BAD_GATEWAY, format!("Failed to connect to upstream: {}", e)).into_response()
        }
    }
}
