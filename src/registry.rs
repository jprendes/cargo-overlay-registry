use crate::state::ProxyState;
use crate::types::{
    IndexDependency, IndexEntry, PublishMetadata, PublishResponse, PublishWarnings, RegistryConfig,
};
use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, HeaderMap, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use bytes::Bytes;
use log::{error, info, warn};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tokio::fs;
use tokio::io::AsyncWriteExt;

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
    headers: HeaderMap,
) -> impl IntoResponse {
    let path = format!("/1/{}", name);
    proxy_index_request(&state, &path, &headers).await
}

/// Handle index request for 2-character package names
pub async fn handle_index_2char(
    State(state): State<Arc<ProxyState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let path = format!("/2/{}", name);
    proxy_index_request(&state, &path, &headers).await
}

/// Handle index request for 3-character package names
pub async fn handle_index_3char(
    State(state): State<Arc<ProxyState>>,
    Path((first_char, name)): Path<(String, String)>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let path = format!("/3/{}/{}", first_char, name);
    proxy_index_request(&state, &path, &headers).await
}

/// Handle index request for 4+ character package names
pub async fn handle_index_4plus(
    State(state): State<Arc<ProxyState>>,
    Path((first_two, second_two, name)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let path = format!("/{}/{}/{}", first_two, second_two, name);
    proxy_index_request(&state, &path, &headers).await
}

/// Proxy an index request to crates.io sparse index
/// Merges local index entries with upstream
async fn proxy_index_request(state: &ProxyState, path: &str, headers: &HeaderMap) -> Response {
    // Determine local index path from the request path
    let local_index_path = state
        .local_registry_path
        .join("index")
        .join(path.trim_start_matches('/'));

    // Read local index entries if they exist
    let local_entries: Vec<String> = if local_index_path.exists() {
        match fs::read_to_string(&local_index_path).await {
            Ok(content) => {
                info!(
                    "  Found local index entries at: {}",
                    local_index_path.display()
                );
                content.lines().map(|s| s.to_string()).collect()
            }
            Err(_) => Vec::new(),
        }
    } else {
        Vec::new()
    };

    let url = format!("{}{}", state.upstream_index, path);
    info!("GET {} -> {}", path, url);

    let mut request = state.client.get(&url);

    // Forward caching headers (but only if we don't have local entries to merge)
    // If we have local entries, we need fresh upstream data to merge properly
    if local_entries.is_empty() {
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
            info!(
                "  Upstream response: {} {}",
                status.as_u16(),
                status.canonical_reason().unwrap_or("")
            );

            // If upstream returns 404 but we have local entries, return those
            if status == reqwest::StatusCode::NOT_FOUND && !local_entries.is_empty() {
                let body = local_entries.join("\n") + "\n";
                info!(
                    "  Returning local entries only ({} entries)",
                    local_entries.len()
                );
                return Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .unwrap();
            }

            let mut builder = Response::builder().status(
                StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            );

            // Forward relevant response headers (but not caching headers if we merged)
            if local_entries.is_empty() {
                if let Some(etag) = response.headers().get(header::ETAG) {
                    builder = builder.header(header::ETAG, etag.clone());
                    info!("  Response ETag: {:?}", etag);
                }
                if let Some(last_modified) = response.headers().get(header::LAST_MODIFIED) {
                    builder = builder.header(header::LAST_MODIFIED, last_modified.clone());
                }
            }
            if let Some(content_type) = response.headers().get(header::CONTENT_TYPE) {
                builder = builder.header(header::CONTENT_TYPE, content_type.clone());
            }

            match response.bytes().await {
                Ok(upstream_body) => {
                    if status.is_success() {
                        // Merge local entries with upstream
                        let upstream_str = String::from_utf8_lossy(&upstream_body);
                        let mut all_entries: Vec<String> = upstream_str
                            .lines()
                            .filter(|l| !l.is_empty())
                            .map(|s| s.to_string())
                            .collect();

                        // Add local entries, avoiding duplicates by version
                        for local_entry in &local_entries {
                            if let Ok(local_parsed) = serde_json::from_str::<IndexEntry>(local_entry)
                            {
                                // Remove any upstream entry with same version
                                all_entries.retain(|e| {
                                    if let Ok(parsed) = serde_json::from_str::<IndexEntry>(e) {
                                        parsed.vers != local_parsed.vers
                                    } else {
                                        true
                                    }
                                });
                                all_entries.push(local_entry.clone());
                            }
                        }

                        let merged_body = all_entries.join("\n") + "\n";
                        if !local_entries.is_empty() {
                            info!(
                                "  Merged {} upstream + {} local entries",
                                upstream_str.lines().count(),
                                local_entries.len()
                            );
                        }
                        if merged_body.len() < 1000 {
                            info!("  Body: {}", merged_body);
                        } else {
                            info!("  Body: {} bytes", merged_body.len());
                        }
                        builder.body(Body::from(merged_body)).unwrap()
                    } else {
                        builder.body(Body::from(upstream_body)).unwrap()
                    }
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
            // If upstream fails but we have local entries, return those
            if !local_entries.is_empty() {
                let body = local_entries.join("\n") + "\n";
                info!(
                    "  Upstream failed, returning local entries only ({} entries)",
                    local_entries.len()
                );
                return Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .unwrap();
            }
            error!("  Failed to connect to upstream: {}", e);
            (
                StatusCode::BAD_GATEWAY,
                format!("Failed to connect to upstream: {}", e),
            )
                .into_response()
        }
    }
}

/// Handle API search request: GET /api/v1/crates
pub async fn handle_api_search(
    State(state): State<Arc<ProxyState>>,
    uri: Uri,
    headers: HeaderMap,
) -> impl IntoResponse {
    let query = uri.query().map(|q| format!("?{}", q)).unwrap_or_default();
    let path = format!("/api/v1/crates{}", query);
    let url = format!("{}{}", state.upstream_api, path);

    info!("GET /api/v1/crates{} -> {}", query, url);
    proxy_api_request(&state, Method::GET, &url, &headers, None).await
}

/// Handle API publish request: PUT /api/v1/crates/new
/// This saves the crate locally instead of proxying to crates.io
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
    // Format: 32-bit LE JSON length + JSON + 32-bit LE crate length + .crate data
    if body.len() < 8 {
        error!("  Request body too short");
        return (StatusCode::BAD_REQUEST, "Request body too short").into_response();
    }

    let json_len = u32::from_le_bytes([body[0], body[1], body[2], body[3]]) as usize;
    if body.len() < 4 + json_len + 4 {
        error!("  Request body too short for metadata");
        return (StatusCode::BAD_REQUEST, "Request body too short").into_response();
    }

    let json_bytes = &body[4..4 + json_len];
    let metadata: PublishMetadata = match serde_json::from_slice(json_bytes) {
        Ok(m) => m,
        Err(e) => {
            error!("  Failed to parse metadata: {}", e);
            return (
                StatusCode::BAD_REQUEST,
                format!("Invalid metadata: {}", e),
            )
                .into_response();
        }
    };

    // Validate metadata unless permissive publishing is enabled
    if !state.permissive_publishing {
        let validation_errors = metadata.validate();
        if !validation_errors.is_empty() {
            let msg = validation_errors.join("; ");
            error!("  Validation failed: {}", msg);
            return (
                StatusCode::BAD_REQUEST,
                format!("Validation failed: {}", msg),
            )
                .into_response();
        }
    }

    info!("  Publishing: {} v{}", metadata.name, metadata.vers);

    let crate_len_offset = 4 + json_len;
    let crate_len = u32::from_le_bytes([
        body[crate_len_offset],
        body[crate_len_offset + 1],
        body[crate_len_offset + 2],
        body[crate_len_offset + 3],
    ]) as usize;

    let crate_data_offset = crate_len_offset + 4;
    if body.len() < crate_data_offset + crate_len {
        error!("  Request body too short for crate data");
        return (StatusCode::BAD_REQUEST, "Request body too short").into_response();
    }

    let crate_data = &body[crate_data_offset..crate_data_offset + crate_len];

    // Compute SHA256 checksum
    let mut hasher = Sha256::new();
    hasher.update(crate_data);
    let checksum = format!("{:x}", hasher.finalize());
    info!("  Checksum: {}", checksum);

    // Save the .crate file
    let crate_dir = state
        .local_registry_path
        .join("crates")
        .join(&metadata.name);
    if let Err(e) = fs::create_dir_all(&crate_dir).await {
        error!("  Failed to create crate directory: {}", e);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to create directory: {}", e),
        )
            .into_response();
    }

    let crate_file = crate_dir.join(format!("{}.crate", metadata.vers));
    match fs::File::create(&crate_file).await {
        Ok(mut file) => {
            if let Err(e) = file.write_all(crate_data).await {
                error!("  Failed to write crate file: {}", e);
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to write crate: {}", e),
                )
                    .into_response();
            }
        }
        Err(e) => {
            error!("  Failed to create crate file: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to create crate file: {}", e),
            )
                .into_response();
        }
    }
    info!("  Saved crate to: {}", crate_file.display());

    // Create index entry
    let index_entry = IndexEntry {
        name: metadata.name.clone(),
        vers: metadata.vers.clone(),
        deps: metadata
            .deps
            .into_iter()
            .map(|d| {
                // Convert publish dependency to index dependency
                // If explicit_name_in_toml is set, that's the alias (goes in "name")
                // and the original name goes in "package"
                let (name, package) = if let Some(alias) = d.explicit_name_in_toml {
                    (alias, Some(d.name))
                } else {
                    (d.name, None)
                };
                IndexDependency {
                    name,
                    req: d.version_req,
                    features: d.features,
                    optional: d.optional,
                    default_features: d.default_features,
                    target: d.target,
                    kind: d.kind,
                    registry: d.registry,
                    package,
                }
            })
            .collect(),
        cksum: checksum,
        features: metadata.features,
        yanked: false,
        links: metadata.links,
        rust_version: metadata.rust_version,
    };

    // Determine index path based on crate name length
    let name_lower = metadata.name.to_lowercase();
    let index_path = match name_lower.len() {
        1 => state
            .local_registry_path
            .join("index")
            .join("1")
            .join(&name_lower),
        2 => state
            .local_registry_path
            .join("index")
            .join("2")
            .join(&name_lower),
        3 => state
            .local_registry_path
            .join("index")
            .join("3")
            .join(&name_lower[..1])
            .join(&name_lower),
        _ => state
            .local_registry_path
            .join("index")
            .join(&name_lower[..2])
            .join(&name_lower[2..4])
            .join(&name_lower),
    };

    // Create parent directories
    if let Some(parent) = index_path.parent() {
        if let Err(e) = fs::create_dir_all(parent).await {
            error!("  Failed to create index directory: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to create index dir: {}", e),
            )
                .into_response();
        }
    }

    // Append to index file (each version is a line)
    let index_line = match serde_json::to_string(&index_entry) {
        Ok(s) => s,
        Err(e) => {
            error!("  Failed to serialize index entry: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to serialize: {}", e),
            )
                .into_response();
        }
    };

    // Read existing index, filter out same version if exists, append new
    let mut lines: Vec<String> = if index_path.exists() {
        match fs::read_to_string(&index_path).await {
            Ok(content) => content
                .lines()
                .filter(|line| {
                    // Remove existing entry for same version
                    if let Ok(entry) = serde_json::from_str::<IndexEntry>(line) {
                        entry.vers != metadata.vers
                    } else {
                        true
                    }
                })
                .map(|s| s.to_string())
                .collect(),
            Err(_) => Vec::new(),
        }
    } else {
        Vec::new()
    };
    lines.push(index_line);

    let index_content = lines.join("\n") + "\n";
    if let Err(e) = fs::write(&index_path, index_content).await {
        error!("  Failed to write index file: {}", e);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to write index: {}", e),
        )
            .into_response();
    }
    info!("  Updated index at: {}", index_path.display());

    let response = PublishResponse {
        warnings: PublishWarnings {
            invalid_categories: vec![],
            invalid_badges: vec![],
            other: vec![],
        },
    };

    info!("  Response: 200 OK");
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::to_string(&response).unwrap(),
    )
        .into_response()
}

/// Handle API download request: GET /api/v1/crates/{crate}/{version}/download
/// Checks local registry first, then falls back to upstream
pub async fn handle_api_download(
    State(state): State<Arc<ProxyState>>,
    Path((crate_name, version)): Path<(String, String)>,
    headers: HeaderMap,
) -> impl IntoResponse {
    // Check local registry first
    let local_crate = state
        .local_registry_path
        .join("crates")
        .join(&crate_name)
        .join(format!("{}.crate", version));

    if local_crate.exists() {
        info!(
            "GET /api/v1/crates/{}/{}/download -> local: {}",
            crate_name,
            version,
            local_crate.display()
        );
        match fs::read(&local_crate).await {
            Ok(data) => {
                info!("  Response: 200 OK ({} bytes from local)", data.len());
                return (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, "application/gzip")],
                    data,
                )
                    .into_response();
            }
            Err(e) => {
                error!("  Failed to read local crate: {}", e);
                // Fall through to upstream
            }
        }
    }

    // Fall back to upstream
    let url = format!(
        "{}/api/v1/crates/{}/{}/download",
        state.upstream_api, crate_name, version
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
