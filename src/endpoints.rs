use std::fmt;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use log::{error, info, warn};

use crate::registry::{Registry, RegistryError};
use crate::state::ProxyState;
use crate::types::{IndexEntry, PublishMetadata, PublishResponse, PublishWarnings, RegistryConfig};

/// Internal response type for HTTP proxy
pub struct InternalResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl InternalResponse {
    fn ok_json(body: impl AsRef<[u8]>) -> Self {
        Self {
            status: 200,
            headers: vec![("content-type".to_string(), "application/json".to_string())],
            body: body.as_ref().to_vec(),
        }
    }

    fn ok_gzip(body: Vec<u8>) -> Self {
        Self {
            status: 200,
            headers: vec![("content-type".to_string(), "application/gzip".to_string())],
            body,
        }
    }

    fn error(status: u16, message: impl Into<String>) -> Self {
        Self {
            status,
            headers: vec![("content-type".to_string(), "text/plain".to_string())],
            body: message.into().into_bytes(),
        }
    }
}

/// Error type for parsing publish requests
#[derive(Debug)]
pub enum ParseError {
    /// Request body is too short
    BodyTooShort,
    /// Failed to parse JSON metadata
    InvalidJson(serde_json::Error),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::BodyTooShort => write!(f, "request body too short"),
            ParseError::InvalidJson(e) => write!(f, "invalid metadata: {}", e),
        }
    }
}

impl std::error::Error for ParseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ParseError::InvalidJson(e) => Some(e),
            _ => None,
        }
    }
}

/// Parse a cargo publish request body.
///
/// The format is:
/// - 4 bytes: JSON metadata length (little-endian u32)
/// - N bytes: JSON metadata
/// - 4 bytes: .crate file length (little-endian u32)
/// - M bytes: .crate file data
///
/// Returns the parsed metadata and a reference to the crate data.
pub fn parse_publish_body(body: &[u8]) -> Result<(PublishMetadata, &[u8]), ParseError> {
    if body.len() < 8 {
        return Err(ParseError::BodyTooShort);
    }

    let json_len = u32::from_le_bytes([body[0], body[1], body[2], body[3]]) as usize;
    if body.len() < 4 + json_len + 4 {
        return Err(ParseError::BodyTooShort);
    }

    let json_bytes = &body[4..4 + json_len];
    let metadata: PublishMetadata =
        serde_json::from_slice(json_bytes).map_err(ParseError::InvalidJson)?;

    let crate_len_offset = 4 + json_len;
    let crate_len = u32::from_le_bytes([
        body[crate_len_offset],
        body[crate_len_offset + 1],
        body[crate_len_offset + 2],
        body[crate_len_offset + 3],
    ]) as usize;

    let crate_data_offset = crate_len_offset + 4;
    if body.len() < crate_data_offset + crate_len {
        return Err(ParseError::BodyTooShort);
    }

    let crate_data = &body[crate_data_offset..crate_data_offset + crate_len];

    Ok((metadata, crate_data))
}

/// Serialize index entries to JSON lines format (one JSON object per line).
///
/// This is the format expected by cargo's sparse registry protocol.
pub fn serialize_index_entries(entries: &[IndexEntry]) -> String {
    entries
        .iter()
        .filter_map(|e| serde_json::to_string(e).ok())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

/// Returns a modified config.json pointing to our proxy
pub async fn handle_config(State(state): State<Arc<ProxyState>>) -> impl IntoResponse {
    info!("GET /config.json - Serving proxy configuration");

    let config = RegistryConfig {
        dl: format!("{}/api/v1/crates", state.proxy_base_url),
        api: state.proxy_base_url.clone(),
        auth_required: None,
    };

    info!("  Response: 200 OK - dl={}, api={}", config.dl, config.api);

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::to_string_pretty(&config).unwrap(),
    )
}

/// Handle index request for 1-character package names
pub async fn handle_index_1char(
    State(state): State<Arc<ProxyState>>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    handle_index_lookup(&state, &name).await
}

/// Handle index request for 2-character package names
pub async fn handle_index_2char(
    State(state): State<Arc<ProxyState>>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    handle_index_lookup(&state, &name).await
}

/// Handle index request for 3-character package names
pub async fn handle_index_3char(
    State(state): State<Arc<ProxyState>>,
    Path((_first_char, name)): Path<(String, String)>,
) -> impl IntoResponse {
    handle_index_lookup(&state, &name).await
}

/// Handle index request for 4+ character package names
pub async fn handle_index_4plus(
    State(state): State<Arc<ProxyState>>,
    Path((_first_two, _second_two, name)): Path<(String, String, String)>,
) -> impl IntoResponse {
    handle_index_lookup(&state, &name).await
}

/// Common handler for index lookups using the Registry trait
async fn handle_index_lookup(state: &ProxyState, crate_name: &str) -> Response {
    info!("GET index/{} - Looking up crate", crate_name);

    match state.registry.lookup(crate_name).await {
        Ok(entries) => {
            if entries.is_empty() {
                info!("  Response: 404 Not Found");
                return (StatusCode::NOT_FOUND, "Not found").into_response();
            }

            let body = serialize_index_entries(&entries);

            info!("  Response: 200 OK ({} entries)", entries.len());
            if body.len() < 1000 {
                info!("  Body: {}", body.trim());
            }

            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body))
                .unwrap()
        }
        Err(e) => {
            error!("  Failed to lookup crate: {}", e);
            (
                StatusCode::BAD_GATEWAY,
                format!("Failed to lookup crate: {}", e),
            )
                .into_response()
        }
    }
}

/// Handle API search request: GET /api/v1/crates
/// This proxies to the upstream API since search is not part of the Registry trait
pub async fn handle_api_search(
    State(state): State<Arc<ProxyState>>,
    uri: Uri,
    headers: HeaderMap,
) -> impl IntoResponse {
    let query = uri.query().map(|q| format!("?{}", q)).unwrap_or_default();
    let url = format!("{}/api/v1/crates{}", state.upstream_api(), query);

    info!("GET /api/v1/crates{} -> {}", query, url);
    proxy_api_request(&state, Method::GET, &url, &headers).await
}

/// Handle API publish request: PUT /api/v1/crates/new
/// This saves the crate locally using the Registry trait
pub async fn handle_api_publish(
    State(state): State<Arc<ProxyState>>,
    _headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    info!(
        "PUT /api/v1/crates/new ({} bytes) - Publishing locally",
        body.len()
    );

    // Parse the publish request body
    let (metadata, crate_data) = match parse_publish_body(&body) {
        Ok(result) => result,
        Err(e) => {
            error!("  Failed to parse publish body: {}", e);
            return (StatusCode::BAD_REQUEST, e.to_string()).into_response();
        }
    };

    info!("  Publishing: {} v{}", metadata.name, metadata.vers);

    // Use the Registry trait to publish
    match state.registry.publish(metadata, crate_data).await {
        Ok(checksum) => {
            info!("  Checksum: {}", checksum);
            info!("  Response: 200 OK");

            let response = PublishResponse {
                warnings: PublishWarnings {
                    invalid_categories: vec![],
                    invalid_badges: vec![],
                    other: vec![],
                },
            };

            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/json")],
                serde_json::to_string(&response).unwrap(),
            )
                .into_response()
        }
        Err(RegistryError::ValidationFailed(errors)) => {
            let msg = errors.join("; ");
            error!("  Validation failed: {}", msg);
            (
                StatusCode::BAD_REQUEST,
                format!("Validation failed: {}", msg),
            )
                .into_response()
        }
        Err(e) => {
            error!("  Failed to publish: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to publish: {}", e),
            )
                .into_response()
        }
    }
}

/// Handle API download request: GET /api/v1/crates/{crate}/{version}/download
/// Uses the Registry trait to check local first, then falls back to upstream
pub async fn handle_api_download(
    State(state): State<Arc<ProxyState>>,
    Path((crate_name, version)): Path<(String, String)>,
) -> impl IntoResponse {
    info!("GET /api/v1/crates/{}/{}/download", crate_name, version);

    match state.registry.download(&crate_name, &version).await {
        Ok(data) => {
            info!("  Response: 200 OK ({} bytes)", data.len());
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/gzip")],
                data,
            )
                .into_response()
        }
        Err(RegistryError::NotFound) => {
            info!("  Response: 404 Not Found");
            (StatusCode::NOT_FOUND, "Crate not found").into_response()
        }
        Err(e) => {
            error!("  Failed to download: {}", e);
            (
                StatusCode::BAD_GATEWAY,
                format!("Failed to download: {}", e),
            )
                .into_response()
        }
    }
}

/// Generic API proxy function for search and other API calls
async fn proxy_api_request(
    state: &ProxyState,
    method: Method,
    url: &str,
    headers: &HeaderMap,
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
    if let Some(auth) = headers.get(header::AUTHORIZATION)
        && let Ok(value) = auth.to_str()
    {
        request = request.header(header::AUTHORIZATION, value);
        info!("  Forwarding Authorization header");
    }

    // Forward accept header
    if let Some(accept) = headers.get(header::ACCEPT)
        && let Ok(value) = accept.to_str()
    {
        request = request.header(header::ACCEPT, value);
    }

    match request.send().await {
        Ok(response) => {
            let status = response.status();
            info!(
                "  Response: {} {}",
                status.as_u16(),
                status.canonical_reason().unwrap_or("")
            );

            let mut builder = Response::builder().status(
                StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            );

            // Forward response headers
            for (key, value) in response.headers().iter() {
                if key != header::TRANSFER_ENCODING && key != header::CONNECTION {
                    builder = builder.header(key.clone(), value.clone());
                }
            }

            match response.bytes().await {
                Ok(body) => {
                    if body.len() < 5000 {
                        if let Ok(text) = std::str::from_utf8(&body)
                            && (text.starts_with('{') || text.starts_with('['))
                        {
                            info!("  Response body: {}", text);
                        }
                    } else {
                        info!("  Response body: {} bytes (binary/large)", body.len());
                    }
                    builder.body(Body::from(body)).unwrap()
                }
                Err(e) => {
                    error!("  Failed to read response body: {}", e);
                    (
                        StatusCode::BAD_GATEWAY,
                        format!("Failed to read upstream response: {}", e),
                    )
                        .into_response()
                }
            }
        }
        Err(e) => {
            error!("  Failed to connect to upstream: {}", e);
            (
                StatusCode::BAD_GATEWAY,
                format!("Failed to connect to upstream: {}", e),
            )
                .into_response()
        }
    }
}

/// Handle an internal request from the HTTP proxy without going through axum.
/// Routes based on method and path, returning an InternalResponse.
pub async fn handle_internal_request(
    state: &ProxyState,
    method: &str,
    path: &str,
    headers: &[(String, String)],
    body: &[u8],
) -> InternalResponse {
    // Route based on path
    match (method, path) {
        ("GET", "/config.json") => internal_handle_config(state),

        ("GET", p) if p.starts_with("/1/") => {
            let name = &p[3..];
            internal_handle_index_lookup(state, name).await
        }

        ("GET", p) if p.starts_with("/2/") => {
            let name = &p[3..];
            internal_handle_index_lookup(state, name).await
        }

        ("GET", p) if p.starts_with("/3/") => {
            // /3/{first_char}/{name}
            let rest = &p[3..];
            if let Some(slash_pos) = rest.find('/') {
                let name = &rest[slash_pos + 1..];
                internal_handle_index_lookup(state, name).await
            } else {
                InternalResponse::error(400, "Invalid path")
            }
        }

        ("GET", p)
            if p.len() > 6 && p.chars().nth(3) == Some('/') && p.chars().nth(6) == Some('/') =>
        {
            // /{first_two}/{second_two}/{name}
            let name = &p[7..];
            internal_handle_index_lookup(state, name).await
        }

        ("GET", p) if p.starts_with("/api/v1/crates/") && p.ends_with("/download") => {
            // /api/v1/crates/{crate}/{version}/download
            let parts: Vec<&str> = p
                .trim_start_matches("/api/v1/crates/")
                .trim_end_matches("/download")
                .split('/')
                .collect();
            if parts.len() == 2 {
                internal_handle_download(state, parts[0], parts[1]).await
            } else {
                InternalResponse::error(400, "Invalid download path")
            }
        }

        ("GET", p) if p.starts_with("/api/v1/crates") => {
            // Search - proxy to upstream
            let query = if let Some(q) = p.strip_prefix("/api/v1/crates") {
                q.to_string()
            } else {
                String::new()
            };
            internal_handle_search(state, &query, headers).await
        }

        ("PUT", "/api/v1/crates/new") => internal_handle_publish(state, body).await,

        _ => InternalResponse::error(404, "Not found"),
    }
}

fn internal_handle_config(state: &ProxyState) -> InternalResponse {
    info!("GET /config.json - Serving proxy configuration (internal)");

    let config = RegistryConfig {
        dl: format!("{}/api/v1/crates", state.proxy_base_url),
        api: state.proxy_base_url.clone(),
        auth_required: None,
    };

    info!("  Response: 200 OK - dl={}, api={}", config.dl, config.api);
    InternalResponse::ok_json(serde_json::to_string_pretty(&config).unwrap())
}

async fn internal_handle_index_lookup(state: &ProxyState, crate_name: &str) -> InternalResponse {
    info!("GET index/{} - Looking up crate (internal)", crate_name);

    match state.registry.lookup(crate_name).await {
        Ok(entries) => {
            if entries.is_empty() {
                info!("  Response: 404 Not Found");
                return InternalResponse::error(404, "Not found");
            }

            let body = serialize_index_entries(&entries);

            info!("  Response: 200 OK ({} entries)", entries.len());
            if body.len() < 1000 {
                info!("  Body: {}", body.trim());
            }

            InternalResponse::ok_json(body)
        }
        Err(e) => {
            error!("  Failed to lookup crate: {}", e);
            InternalResponse::error(502, format!("Failed to lookup crate: {}", e))
        }
    }
}

async fn internal_handle_publish(state: &ProxyState, body: &[u8]) -> InternalResponse {
    info!(
        "PUT /api/v1/crates/new ({} bytes) - Publishing locally (internal)",
        body.len()
    );

    // Parse the publish request body
    let (metadata, crate_data) = match parse_publish_body(body) {
        Ok(result) => result,
        Err(e) => {
            error!("  Failed to parse publish body: {}", e);
            return InternalResponse::error(400, e.to_string());
        }
    };

    info!("  Publishing: {} v{}", metadata.name, metadata.vers);

    // Use the Registry trait to publish
    match state.registry.publish(metadata, crate_data).await {
        Ok(checksum) => {
            info!("  Checksum: {}", checksum);
            info!("  Response: 200 OK");

            let response = PublishResponse {
                warnings: PublishWarnings {
                    invalid_categories: vec![],
                    invalid_badges: vec![],
                    other: vec![],
                },
            };

            InternalResponse::ok_json(serde_json::to_string(&response).unwrap())
        }
        Err(RegistryError::ValidationFailed(errors)) => {
            let msg = errors.join("; ");
            error!("  Validation failed: {}", msg);
            InternalResponse::error(400, format!("Validation failed: {}", msg))
        }
        Err(e) => {
            error!("  Failed to publish: {}", e);
            InternalResponse::error(500, format!("Failed to publish: {}", e))
        }
    }
}

async fn internal_handle_download(
    state: &ProxyState,
    crate_name: &str,
    version: &str,
) -> InternalResponse {
    info!(
        "GET /api/v1/crates/{}/{}/download (internal)",
        crate_name, version
    );

    match state.registry.download(crate_name, version).await {
        Ok(data) => {
            info!("  Response: 200 OK ({} bytes)", data.len());
            InternalResponse::ok_gzip(data)
        }
        Err(RegistryError::NotFound) => {
            info!("  Response: 404 Not Found");
            InternalResponse::error(404, "Crate not found")
        }
        Err(e) => {
            error!("  Failed to download: {}", e);
            InternalResponse::error(502, format!("Failed to download: {}", e))
        }
    }
}

async fn internal_handle_search(
    state: &ProxyState,
    query: &str,
    headers: &[(String, String)],
) -> InternalResponse {
    let url = format!("{}/api/v1/crates{}", state.upstream_api(), query);
    info!("GET /api/v1/crates{} -> {} (internal)", query, url);

    let mut request = state.client.get(&url);

    // Forward authorization header
    for (name, value) in headers {
        if name.to_lowercase() == "authorization" {
            request = request.header("Authorization", value);
            info!("  Forwarding Authorization header");
        } else if name.to_lowercase() == "accept" {
            request = request.header("Accept", value);
        }
    }

    match request.send().await {
        Ok(response) => {
            let status = response.status();
            info!(
                "  Response: {} {}",
                status.as_u16(),
                status.canonical_reason().unwrap_or("")
            );

            let mut resp_headers = Vec::new();
            for (key, value) in response.headers().iter() {
                if key != "transfer-encoding"
                    && key != "connection"
                    && let Ok(v) = value.to_str()
                {
                    resp_headers.push((key.to_string(), v.to_string()));
                }
            }

            match response.bytes().await {
                Ok(body) => InternalResponse {
                    status: status.as_u16(),
                    headers: resp_headers,
                    body: body.to_vec(),
                },
                Err(e) => {
                    error!("  Failed to read response body: {}", e);
                    InternalResponse::error(502, format!("Failed to read upstream response: {}", e))
                }
            }
        }
        Err(e) => {
            error!("  Failed to connect to upstream: {}", e);
            InternalResponse::error(502, format!("Failed to connect to upstream: {}", e))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_publish_body_too_short() {
        assert!(matches!(
            parse_publish_body(&[0, 0, 0, 0]),
            Err(ParseError::BodyTooShort)
        ));
    }

    #[test]
    fn test_parse_publish_body_invalid_json() {
        // JSON length = 4, but contains invalid JSON
        let body = [
            4, 0, 0, 0, // JSON length: 4
            b'n', b'o', b'p', b'e', // Invalid JSON
            0, 0, 0, 0, // Crate length: 0
        ];
        assert!(matches!(
            parse_publish_body(&body),
            Err(ParseError::InvalidJson(_))
        ));
    }

    #[test]
    fn test_serialize_empty_entries() {
        let entries: Vec<IndexEntry> = vec![];
        assert_eq!(serialize_index_entries(&entries), "\n");
    }
}
