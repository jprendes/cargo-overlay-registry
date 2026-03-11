use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, HeaderMap, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{get, put},
    Router,
};
use axum_server::tls_rustls::RustlsConfig;
use bytes::Bytes;
use clap::Parser;
use log::{debug, error, info, warn};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;

/// Cargo registry proxy - proxies crates.io and supports local publishing
#[derive(Parser, Debug)]
#[command(name = "cargo-proxy-registry")]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Port to listen on
    #[arg(short, long, default_value = "8080")]
    port: u16,

    /// Host/IP to bind to
    #[arg(short = 'H', long, default_value = "0.0.0.0")]
    host: String,

    /// Base URL for the proxy (used in config.json)
    /// Defaults to http://localhost:<port>
    #[arg(short, long)]
    base_url: Option<String>,

    /// Path to store locally published crates
    #[arg(short, long, default_value = "./local-registry")]
    registry_path: PathBuf,

    /// Upstream registry sparse index URL
    #[arg(long, default_value = "https://index.crates.io")]
    upstream_index: String,

    /// Upstream registry API URL
    #[arg(long, default_value = "https://crates.io")]
    upstream_api: String,

    /// Optional HTTP proxy port (for CARGO_HTTP_PROXY)
    /// When set, starts an HTTP forward proxy that intercepts traffic
    #[arg(long)]
    http_proxy_port: Option<u16>,

    /// Path to export CA certificate (PEM format) for MITM interception
    /// Use with CARGO_HTTP_CAINFO to make cargo trust the proxy's certificates
    #[arg(long)]
    ca_cert_out: Option<PathBuf>,

    /// Path to TLS certificate file (PEM format)
    /// If not provided but --tls is set, a self-signed certificate will be generated
    #[arg(long)]
    tls_cert: Option<PathBuf>,

    /// Path to TLS private key file (PEM format)
    /// Required if --tls-cert is provided
    #[arg(long)]
    tls_key: Option<PathBuf>,

    /// Enable HTTPS with self-signed certificate (if --tls-cert not provided)
    #[arg(long)]
    tls: bool,
}

/// Proxy state containing the HTTP client
struct ProxyState {
    client: Client,
    /// The base URL where this proxy is listening (for config.json rewriting)
    proxy_base_url: String,
    /// Local registry storage path
    local_registry_path: PathBuf,
    /// Upstream registry sparse index URL
    upstream_index: String,
    /// Upstream registry API URL
    upstream_api: String,
}

/// CA certificate for MITM TLS interception
struct MitmCa {
    /// CA certificate in PEM format
    ca_cert_pem: Vec<u8>,
    /// CA key pair for signing domain certificates
    ca_key_pair: rcgen::KeyPair,
    /// CA certificate for signing
    ca_cert: rcgen::Certificate,
}

impl MitmCa {
    /// Generate a new CA certificate
    fn new() -> Result<Self, rcgen::Error> {
        use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, KeyPair, KeyUsagePurpose};
        
        let mut params = CertificateParams::default();
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::CrlSign,
        ];
        params.distinguished_name.push(DnType::CommonName, "Cargo Proxy Registry CA");
        params.distinguished_name.push(DnType::OrganizationName, "Cargo Proxy Registry");
        
        let key_pair = KeyPair::generate()?;
        let ca_cert = params.self_signed(&key_pair)?;
        
        let ca_cert_pem = ca_cert.pem().into_bytes();
        
        Ok(Self {
            ca_cert_pem,
            ca_key_pair: key_pair,
            ca_cert,
        })
    }
    
    /// Generate a certificate for a domain, signed by this CA
    fn sign_domain_cert(&self, domain: &str) -> Result<(Vec<u8>, Vec<u8>), rcgen::Error> {
        use rcgen::{CertificateParams, DnType, KeyPair, SanType};
        
        let mut params = CertificateParams::default();
        params.distinguished_name.push(DnType::CommonName, domain);
        params.subject_alt_names = vec![
            SanType::DnsName(domain.try_into().map_err(|_| rcgen::Error::CouldNotParseCertificate)?),
        ];
        
        // Add wildcard if domain has subdomains potential
        if !domain.starts_with("*.") {
            if let Ok(wildcard) = format!("*.{}", domain).try_into() {
                params.subject_alt_names.push(SanType::DnsName(wildcard));
            }
        }
        
        let key_pair = KeyPair::generate()?;
        let cert = params.signed_by(&key_pair, &self.ca_cert, &self.ca_key_pair)?;
        
        let cert_pem = cert.pem().into_bytes();
        let key_pem = key_pair.serialize_pem().into_bytes();
        
        Ok((cert_pem, key_pem))
    }
}

/// Publish request metadata (from cargo)
#[derive(Deserialize, Debug)]
#[allow(dead_code)]
struct PublishMetadata {
    name: String,
    vers: String,
    #[serde(default)]
    deps: Vec<PublishDependency>,
    #[serde(default)]
    features: std::collections::HashMap<String, Vec<String>>,
    #[serde(default)]
    authors: Vec<String>,
    description: Option<String>,
    documentation: Option<String>,
    homepage: Option<String>,
    readme: Option<String>,
    readme_file: Option<String>,
    #[serde(default)]
    keywords: Vec<String>,
    #[serde(default)]
    categories: Vec<String>,
    license: Option<String>,
    license_file: Option<String>,
    repository: Option<String>,
    links: Option<String>,
    rust_version: Option<String>,
}

/// Dependency in publish request
#[derive(Deserialize, Debug)]
struct PublishDependency {
    name: String,
    version_req: String,
    #[serde(default)]
    features: Vec<String>,
    #[serde(default)]
    optional: bool,
    #[serde(default = "default_true")]
    default_features: bool,
    target: Option<String>,
    kind: Option<String>,
    registry: Option<String>,
    explicit_name_in_toml: Option<String>,
}

fn default_true() -> bool {
    true
}

/// Index entry for a crate version
#[derive(Serialize, Deserialize, Debug)]
struct IndexEntry {
    name: String,
    vers: String,
    deps: Vec<IndexDependency>,
    cksum: String,
    features: std::collections::HashMap<String, Vec<String>>,
    #[serde(default)]
    yanked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    links: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rust_version: Option<String>,
}

/// Dependency in index entry
#[derive(Serialize, Deserialize, Debug)]
struct IndexDependency {
    name: String,
    req: String,
    features: Vec<String>,
    optional: bool,
    default_features: bool,
    target: Option<String>,
    kind: Option<String>,
    registry: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    package: Option<String>,
}

/// Publish response
#[derive(Serialize)]
struct PublishResponse {
    warnings: PublishWarnings,
}

#[derive(Serialize)]
struct PublishWarnings {
    invalid_categories: Vec<String>,
    invalid_badges: Vec<String>,
    other: Vec<String>,
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

    let args = Args::parse();

    // Determine protocol and base URL
    let use_tls = args.tls || args.tls_cert.is_some();
    let protocol = if use_tls { "https" } else { "http" };
    
    let proxy_base_url = args.base_url
        .unwrap_or_else(|| format!("{}://localhost:{}", protocol, args.port));

    let local_registry_path = args.registry_path;

    info!("Starting cargo registry proxy on {}:{}", args.host, args.port);
    info!("Proxy base URL: {}", proxy_base_url);
    info!("Local registry path: {}", local_registry_path.display());
    info!("Proxying index from: {}", args.upstream_index);
    info!("Proxying API from: {}", args.upstream_api);
    if use_tls {
        info!("TLS enabled");
    }

    // Create local registry directories
    fs::create_dir_all(local_registry_path.join("crates")).await.ok();
    fs::create_dir_all(local_registry_path.join("index")).await.ok();

    // Extract hosts from upstream URLs for HTTP proxy interception (before args are moved)
    let upstream_hosts: Vec<String> = [
        &args.upstream_index,
        &args.upstream_api,
    ]
    .iter()
    .filter_map(|url_str| {
        url::Url::parse(url_str).ok().and_then(|u| u.host_str().map(|h| h.to_string()))
    })
    .collect();

    let state = Arc::new(ProxyState {
        client: Client::builder()
            .user_agent("cargo-proxy-registry/0.1.0")
            .build()
            .expect("Failed to create HTTP client"),
        proxy_base_url: proxy_base_url.clone(),
        local_registry_path,
        upstream_index: args.upstream_index,
        upstream_api: args.upstream_api,
    });

    let app = Router::new()
        // Index config endpoint
        .route("/config.json", get(handle_config))
        // Index files for 1-char package names: /1/{name}
        .route("/1/{name}", get(handle_index_1char))
        // Index files for 2-char package names: /2/{name}
        .route("/2/{name}", get(handle_index_2char))
        // Index files for 3-char package names: /3/{first_char}/{name}
        .route("/3/{first_char}/{name}", get(handle_index_3char))
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

    let bind_addr = format!("{}:{}", args.host, args.port);

    info!("Listening on {}", bind_addr);
    info!("Configure cargo to use: sparse+{}/", proxy_base_url);

    // Start HTTP proxy if configured
    if let Some(http_proxy_port) = args.http_proxy_port {
        let http_proxy_addr = format!("{}:{}", args.host, http_proxy_port);
        let main_proxy_host = args.host.clone();
        let main_proxy_port = args.port;
        
        info!("Intercepting hosts: {:?}", upstream_hosts);
        
        // Generate MITM CA certificate
        let mitm_ca = Arc::new(MitmCa::new().expect("Failed to generate MITM CA certificate"));
        
        // Export CA certificate if requested
        if let Some(ca_cert_path) = &args.ca_cert_out {
            std::fs::write(ca_cert_path, &mitm_ca.ca_cert_pem)
                .expect("Failed to write CA certificate");
            info!("Exported CA certificate to {:?}", ca_cert_path);
            info!("Set CARGO_HTTP_CAINFO={:?} to trust the proxy's certificates", ca_cert_path);
        }
        
        info!("Starting HTTP proxy on {}", http_proxy_addr);
        info!("Set CARGO_HTTP_PROXY=http://{} to route traffic through proxy", http_proxy_addr);
        
        tokio::spawn(async move {
            run_http_proxy(&http_proxy_addr, &main_proxy_host, main_proxy_port, mitm_ca, upstream_hosts).await;
        });
    }

    if use_tls {
        // Load or generate TLS configuration
        let tls_config = if let (Some(cert_path), Some(key_path)) = (&args.tls_cert, &args.tls_key) {
            info!("Loading TLS certificate from {:?}", cert_path);
            info!("Loading TLS key from {:?}", key_path);
            RustlsConfig::from_pem_file(cert_path, key_path)
                .await
                .expect("Failed to load TLS certificate/key")
        } else {
            info!("Generating self-signed TLS certificate");
            let (cert_pem, key_pem) = generate_self_signed_cert(&args.host)
                .expect("Failed to generate self-signed certificate");
            RustlsConfig::from_pem(cert_pem, key_pem)
                .await
                .expect("Failed to create TLS config from self-signed cert")
        };

        let addr: std::net::SocketAddr = bind_addr.parse().expect("Invalid bind address");
        axum_server::bind_rustls(addr, tls_config)
            .serve(app.into_make_service())
            .await
            .expect("Server error");
    } else {
        let listener = tokio::net::TcpListener::bind(&bind_addr)
            .await
            .expect("Failed to bind to port");
        axum::serve(listener, app).await.expect("Server error");
    }
}

/// Generate a self-signed certificate for the given hostname
fn generate_self_signed_cert(hostname: &str) -> Result<(Vec<u8>, Vec<u8>), rcgen::Error> {
    use rcgen::{CertifiedKey, generate_simple_self_signed};
    
    let subject_alt_names = vec![
        hostname.to_string(),
        "localhost".to_string(),
        "127.0.0.1".to_string(),
    ];
    
    let CertifiedKey { cert, key_pair } = generate_simple_self_signed(subject_alt_names)?;
    
    let cert_pem = cert.pem().into_bytes();
    let key_pem = key_pair.serialize_pem().into_bytes();
    
    Ok((cert_pem, key_pem))
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

/// Handle index request for 1-character package names
async fn handle_index_1char(
    State(state): State<Arc<ProxyState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let path = format!("/1/{}", name);
    proxy_index_request(&state, &path, &headers).await
}

/// Handle index request for 2-character package names
async fn handle_index_2char(
    State(state): State<Arc<ProxyState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let path = format!("/2/{}", name);
    proxy_index_request(&state, &path, &headers).await
}

/// Handle index request for 3-character package names
async fn handle_index_3char(
    State(state): State<Arc<ProxyState>>,
    Path((first_char, name)): Path<(String, String)>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let path = format!("/3/{}/{}", first_char, name);
    proxy_index_request(&state, &path, &headers).await
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
/// Merges local index entries with upstream
async fn proxy_index_request(
    state: &ProxyState,
    path: &str,
    headers: &HeaderMap,
) -> Response {
    // Determine local index path from the request path
    let local_index_path = state.local_registry_path.join("index").join(path.trim_start_matches('/'));
    
    // Read local index entries if they exist
    let local_entries: Vec<String> = if local_index_path.exists() {
        match fs::read_to_string(&local_index_path).await {
            Ok(content) => {
                info!("  Found local index entries at: {}", local_index_path.display());
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
            info!("  Upstream response: {} {}", status.as_u16(), status.canonical_reason().unwrap_or(""));

            // If upstream returns 404 but we have local entries, return those
            if status == reqwest::StatusCode::NOT_FOUND && !local_entries.is_empty() {
                let body = local_entries.join("\n") + "\n";
                info!("  Returning local entries only ({} entries)", local_entries.len());
                return Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .unwrap();
            }

            let mut builder = Response::builder().status(StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR));

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
                            if let Ok(local_parsed) = serde_json::from_str::<IndexEntry>(local_entry) {
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
                            info!("  Merged {} upstream + {} local entries", 
                                upstream_str.lines().count(), local_entries.len());
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
                    (StatusCode::BAD_GATEWAY, format!("Failed to read upstream response: {}", e)).into_response()
                }
            }
        }
        Err(e) => {
            // If upstream fails but we have local entries, return those
            if !local_entries.is_empty() {
                let body = local_entries.join("\n") + "\n";
                info!("  Upstream failed, returning local entries only ({} entries)", local_entries.len());
                return Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .unwrap();
            }
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
    let url = format!("{}{}", state.upstream_api, path);

    info!("GET /api/v1/crates{} -> {}", query, url);
    proxy_api_request(&state, Method::GET, &url, &headers, None).await
}

/// Handle API publish request: PUT /api/v1/crates/new
/// This saves the crate locally instead of proxying to crates.io
async fn handle_api_publish(
    State(state): State<Arc<ProxyState>>,
    _headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    info!("PUT /api/v1/crates/new ({} bytes) - Publishing locally", body.len());

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
            return (StatusCode::BAD_REQUEST, format!("Invalid metadata: {}", e)).into_response();
        }
    };

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
    let crate_dir = state.local_registry_path.join("crates").join(&metadata.name);
    if let Err(e) = fs::create_dir_all(&crate_dir).await {
        error!("  Failed to create crate directory: {}", e);
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to create directory: {}", e)).into_response();
    }

    let crate_file = crate_dir.join(format!("{}.crate", metadata.vers));
    match fs::File::create(&crate_file).await {
        Ok(mut file) => {
            if let Err(e) = file.write_all(crate_data).await {
                error!("  Failed to write crate file: {}", e);
                return (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to write crate: {}", e)).into_response();
            }
        }
        Err(e) => {
            error!("  Failed to create crate file: {}", e);
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to create crate file: {}", e)).into_response();
        }
    }
    info!("  Saved crate to: {}", crate_file.display());

    // Create index entry
    let index_entry = IndexEntry {
        name: metadata.name.clone(),
        vers: metadata.vers.clone(),
        deps: metadata.deps.into_iter().map(|d| {
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
        }).collect(),
        cksum: checksum,
        features: metadata.features,
        yanked: false,
        links: metadata.links,
        rust_version: metadata.rust_version,
    };

    // Determine index path based on crate name length
    let name_lower = metadata.name.to_lowercase();
    let index_path = match name_lower.len() {
        1 => state.local_registry_path.join("index").join("1").join(&name_lower),
        2 => state.local_registry_path.join("index").join("2").join(&name_lower),
        3 => state.local_registry_path.join("index").join("3").join(&name_lower[..1]).join(&name_lower),
        _ => state.local_registry_path.join("index")
            .join(&name_lower[..2])
            .join(&name_lower[2..4])
            .join(&name_lower),
    };

    // Create parent directories
    if let Some(parent) = index_path.parent() {
        if let Err(e) = fs::create_dir_all(parent).await {
            error!("  Failed to create index directory: {}", e);
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to create index dir: {}", e)).into_response();
        }
    }

    // Append to index file (each version is a line)
    let index_line = match serde_json::to_string(&index_entry) {
        Ok(s) => s,
        Err(e) => {
            error!("  Failed to serialize index entry: {}", e);
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to serialize: {}", e)).into_response();
        }
    };

    // Read existing index, filter out same version if exists, append new
    let mut lines: Vec<String> = if index_path.exists() {
        match fs::read_to_string(&index_path).await {
            Ok(content) => content.lines()
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
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to write index: {}", e)).into_response();
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
    (StatusCode::OK, [(header::CONTENT_TYPE, "application/json")], serde_json::to_string(&response).unwrap()).into_response()
}

/// Handle API download request: GET /api/v1/crates/{crate}/{version}/download
/// Checks local registry first, then falls back to upstream
async fn handle_api_download(
    State(state): State<Arc<ProxyState>>,
    Path((crate_name, version)): Path<(String, String)>,
    headers: HeaderMap,
) -> impl IntoResponse {
    // Check local registry first
    let local_crate = state.local_registry_path
        .join("crates")
        .join(&crate_name)
        .join(format!("{}.crate", version));

    if local_crate.exists() {
        info!(
            "GET /api/v1/crates/{}/{}/download -> local: {}",
            crate_name, version, local_crate.display()
        );
        match fs::read(&local_crate).await {
            Ok(data) => {
                info!("  Response: 200 OK ({} bytes from local)", data.len());
                return (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, "application/gzip")],
                    data,
                ).into_response();
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

/// Run the HTTP forward proxy
async fn run_http_proxy(bind_addr: &str, main_proxy_host: &str, main_proxy_port: u16, mitm_ca: Arc<MitmCa>, upstream_hosts: Vec<String>) {
    let listener = match tokio::net::TcpListener::bind(bind_addr).await {
        Ok(l) => l,
        Err(e) => {
            error!("Failed to bind HTTP proxy to {}: {}", bind_addr, e);
            return;
        }
    };

    info!("HTTP proxy listening on {}", bind_addr);
    let upstream_hosts = Arc::new(upstream_hosts);

    loop {
        match listener.accept().await {
            Ok((stream, addr)) => {
                debug!("HTTP proxy: connection from {}", addr);
                let main_host = main_proxy_host.to_string();
                let main_port = main_proxy_port;
                let ca = mitm_ca.clone();
                let hosts = upstream_hosts.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_http_proxy_connection(stream, &main_host, main_port, ca, hosts).await {
                        debug!("HTTP proxy connection error: {}", e);
                    }
                });
            }
            Err(e) => {
                error!("HTTP proxy accept error: {}", e);
            }
        }
    }
}

/// Handle a single HTTP proxy connection (supports keep-alive for multiple requests)
async fn handle_http_proxy_connection(
    stream: TcpStream,
    main_proxy_host: &str,
    main_proxy_port: u16,
    mitm_ca: Arc<MitmCa>,
    upstream_hosts: Arc<Vec<String>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use tokio::io::{AsyncBufReadExt, BufReader};
    
    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut buf_reader = BufReader::new(read_half);
    
    // Create a shared reqwest client for all requests on this connection
    let client = reqwest::Client::builder()
        .user_agent("cargo-proxy-registry-http-proxy/0.1.0")
        .build()?;
    
    loop {
        // Read the HTTP request line
        let mut request_line = String::new();
        match buf_reader.read_line(&mut request_line).await {
            Ok(0) => break, // Connection closed
            Ok(_) => {}
            Err(e) => {
                debug!("Error reading from HTTP proxy stream: {}", e);
                break;
            }
        }
        
        if request_line.trim().is_empty() {
            continue;
        }

        debug!("HTTP proxy request: {}", request_line.trim());

        let parts: Vec<&str> = request_line.trim().split_whitespace().collect();
        if parts.len() < 3 {
            write_half.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n").await?;
            break;
        }

        let method = parts[0].to_string();
        let target = parts[1].to_string();

        // Read headers
        let mut headers = Vec::new();
        loop {
            let mut line = String::new();
            match buf_reader.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => {
                    if line.trim().is_empty() {
                        break;
                    }
                    headers.push(line.trim().to_string());
                }
                Err(_) => break,
            }
        }

        if method == "CONNECT" {
            // For CONNECT, we need to reunite the stream and hand off to tunnel handler
            // This consumes the connection, so we return after handling
            let inner = buf_reader.into_inner();
            let stream = inner.unsplit(write_half);
            return handle_connect_tunnel(stream, &target, main_proxy_host, main_proxy_port, mitm_ca, upstream_hosts).await;
        }
        
        // Handle regular HTTP request with keep-alive support
        let should_close = handle_http_forward_request(
            &mut buf_reader,
            &mut write_half,
            &client,
            &method,
            &target,
            &headers,
            main_proxy_host,
            main_proxy_port,
            &upstream_hosts,
        ).await?;
        
        if should_close {
            break;
        }
    }
    
    Ok(())
}

/// Handle CONNECT method (HTTPS tunneling)
/// For upstream registry domains, we perform MITM TLS interception to route through our proxy
async fn handle_connect_tunnel(
    stream: TcpStream,
    target: &str,
    main_proxy_host: &str,
    main_proxy_port: u16,
    mitm_ca: Arc<MitmCa>,
    upstream_hosts: Arc<Vec<String>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Parse target as host:port
    let (host, port) = if let Some(colon_pos) = target.rfind(':') {
        let h = &target[..colon_pos];
        let p: u16 = target[colon_pos + 1..].parse().unwrap_or(443);
        (h, p)
    } else {
        (target, 443u16)
    };

    // Check if this is an upstream registry domain that we should intercept
    let should_intercept = upstream_hosts.iter().any(|upstream_host| {
        host == upstream_host || host.ends_with(&format!(".{}", upstream_host))
    });

    if should_intercept {
        info!("HTTP proxy CONNECT MITM interception for {}:{}", host, port);
        handle_connect_mitm(stream, host, main_proxy_host, main_proxy_port, mitm_ca).await
    } else {
        info!("HTTP proxy CONNECT tunnel to {}:{}", host, port);
        handle_connect_passthrough(stream, host, port).await
    }
}

/// Handle CONNECT with MITM TLS interception for upstream registry domains
async fn handle_connect_mitm(
    mut stream: TcpStream,
    host: &str,
    main_proxy_host: &str,
    main_proxy_port: u16,
    mitm_ca: Arc<MitmCa>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use tokio_rustls::TlsAcceptor;
    use rustls::ServerConfig;
    use tokio::io::{AsyncBufReadExt, BufReader};

    // Generate a certificate for the target domain, signed by our CA
    let (cert_pem, key_pem) = mitm_ca.sign_domain_cert(host)?;
    
    // Parse the certificate and key
    let certs = rustls_pemfile::certs(&mut cert_pem.as_slice())
        .collect::<Result<Vec<_>, _>>()?;
    let key = rustls_pemfile::private_key(&mut key_pem.as_slice())?
        .ok_or("No private key found")?;

    // Build TLS server config
    let server_config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)?;

    let acceptor = TlsAcceptor::from(std::sync::Arc::new(server_config));

    // Send 200 Connection Established before TLS handshake
    stream.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await?;

    // Perform TLS handshake with client
    let tls_stream = match acceptor.accept(stream).await {
        Ok(s) => s,
        Err(e) => {
            error!("TLS handshake failed for {}: {}", host, e);
            return Err(e.into());
        }
    };

    info!("  TLS handshake completed for {}", host);

    // Create a shared reqwest client for all requests on this connection
    let client = reqwest::Client::builder()
        .user_agent("cargo-proxy-registry-mitm/0.1.0")
        .build()?;

    // Split the TLS stream for reading and writing
    let (read_half, mut write_half) = tokio::io::split(tls_stream);
    let mut buf_reader = BufReader::new(read_half);

    // Now handle HTTP requests over the TLS connection
    loop {
        // Read the HTTP request line
        let mut request_line = String::new();
        
        match buf_reader.read_line(&mut request_line).await {
            Ok(0) => break, // Connection closed
            Ok(_) => {}
            Err(e) => {
                debug!("Error reading from TLS stream: {}", e);
                break;
            }
        }

        if request_line.trim().is_empty() {
            // Empty line might just be keep-alive probe, continue
            continue;
        }

        // Parse request line
        let parts: Vec<&str> = request_line.trim().split_whitespace().collect();
        if parts.len() < 3 {
            debug!("Invalid request line: {}", request_line.trim());
            break;
        }

        let method = parts[0];
        let path = parts[1];

        // Read headers
        let mut headers = Vec::new();
        loop {
            let mut line = String::new();
            match buf_reader.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => {
                    if line.trim().is_empty() {
                        break;
                    }
                    headers.push(line.trim().to_string());
                }
                Err(_) => break,
            }
        }

        // Check for Expect: 100-continue header
        let expects_continue = headers.iter()
            .any(|h| h.to_lowercase().starts_with("expect:") && h.to_lowercase().contains("100-continue"));
        
        // Send 100 Continue response if requested before reading body
        if expects_continue {
            tokio::io::AsyncWriteExt::write_all(&mut write_half, b"HTTP/1.1 100 Continue\r\n\r\n").await?;
            tokio::io::AsyncWriteExt::flush(&mut write_half).await?;
            debug!("  Sent 100 Continue for {}", path);
        }

        // Get content length
        let content_length: usize = headers.iter()
            .find(|h| h.to_lowercase().starts_with("content-length:"))
            .and_then(|h| h.split(':').nth(1))
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);

        // Read body if present
        let body = if content_length > 0 {
            let mut body = vec![0u8; content_length];
            tokio::io::AsyncReadExt::read_exact(&mut buf_reader, &mut body).await?;
            body
        } else {
            Vec::new()
        };

        info!("  MITM {} https://{}{}", method, host, path);

        // Rewrite to our proxy
        let proxy_url = format!("http://{}:{}{}", main_proxy_host, main_proxy_port, path);
        info!("    -> Rewriting to: {}", proxy_url);

        let request = match method {
            "GET" => client.get(&proxy_url),
            "POST" => client.post(&proxy_url).body(body),
            "PUT" => client.put(&proxy_url).body(body),
            "DELETE" => client.delete(&proxy_url),
            "HEAD" => client.head(&proxy_url),
            _ => {
                let response = b"HTTP/1.1 405 Method Not Allowed\r\n\r\n";
                tokio::io::AsyncWriteExt::write_all(&mut write_half, response).await?;
                continue;
            }
        };

        // Forward relevant headers
        let mut request = request;
        for header in &headers {
            if let Some(colon_pos) = header.find(':') {
                let name = header[..colon_pos].trim();
                let value = header[colon_pos + 1..].trim();
                if !["host", "connection", "content-length"].contains(&name.to_lowercase().as_str()) {
                    request = request.header(name, value);
                }
            }
        }

        match request.send().await {
            Ok(response) => {
                let status = response.status();
                let mut response_headers = String::new();
                
                for (key, value) in response.headers().iter() {
                    if key != "transfer-encoding" && key != "connection" {
                        response_headers.push_str(&format!("{}: {}\r\n", key, value.to_str().unwrap_or("")));
                    }
                }

                let body = response.bytes().await.unwrap_or_default();
                
                let status_line = format!("HTTP/1.1 {} {}\r\n", status.as_u16(), status.canonical_reason().unwrap_or("OK"));
                let content_length_header = format!("content-length: {}\r\n", body.len());
                
                tokio::io::AsyncWriteExt::write_all(&mut write_half, status_line.as_bytes()).await?;
                tokio::io::AsyncWriteExt::write_all(&mut write_half, response_headers.as_bytes()).await?;
                tokio::io::AsyncWriteExt::write_all(&mut write_half, content_length_header.as_bytes()).await?;
                tokio::io::AsyncWriteExt::write_all(&mut write_half, b"connection: keep-alive\r\n").await?;
                tokio::io::AsyncWriteExt::write_all(&mut write_half, b"\r\n").await?;
                tokio::io::AsyncWriteExt::write_all(&mut write_half, &body).await?;
                tokio::io::AsyncWriteExt::flush(&mut write_half).await?;
                
                info!("    <- {} ({} bytes)", status.as_u16(), body.len());
            }
            Err(e) => {
                error!("    <- Error: {}", e);
                let response = b"HTTP/1.1 502 Bad Gateway\r\nconnection: keep-alive\r\ncontent-length: 0\r\n\r\n";
                tokio::io::AsyncWriteExt::write_all(&mut write_half, response).await?;
                tokio::io::AsyncWriteExt::flush(&mut write_half).await?;
            }
        }

        // Check for Connection: close
        let should_close = headers.iter()
            .any(|h| h.to_lowercase().starts_with("connection:") && h.to_lowercase().contains("close"));
        
        if should_close {
            break;
        }
    }

    Ok(())
}

/// Handle CONNECT with direct passthrough (no interception)
async fn handle_connect_passthrough(
    mut stream: TcpStream,
    host: &str,
    port: u16,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let upstream_addr = format!("{}:{}", host, port);
    match TcpStream::connect(&upstream_addr).await {
        Ok(upstream) => {
            stream.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await?;

            let (mut client_read, mut client_write) = tokio::io::split(stream);
            let (mut upstream_read, mut upstream_write) = tokio::io::split(upstream);

            tokio::select! {
                result = tokio::io::copy(&mut client_read, &mut upstream_write) => {
                    if let Err(e) = result {
                        debug!("CONNECT tunnel client->upstream error: {}", e);
                    }
                }
                result = tokio::io::copy(&mut upstream_read, &mut client_write) => {
                    if let Err(e) = result {
                        debug!("CONNECT tunnel upstream->client error: {}", e);
                    }
                }
            }
        }
        Err(e) => {
            error!("HTTP proxy: failed to connect to {}: {}", upstream_addr, e);
            stream.write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n").await?;
        }
    }

    Ok(())
}

/// Handle regular HTTP request (forward proxy) - returns true if connection should close
async fn handle_http_forward_request<R, W>(
    buf_reader: &mut tokio::io::BufReader<R>,
    write_half: &mut W,
    client: &reqwest::Client,
    method: &str,
    target: &str,
    headers: &[String],
    main_proxy_host: &str,
    main_proxy_port: u16,
    upstream_hosts: &[String],
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::AsyncWriteExt;
    
    // Parse the target URL
    let url = if target.starts_with("http://") || target.starts_with("https://") {
        target.to_string()
    } else {
        // Relative URL - need Host header
        let host = headers.iter()
            .find(|h| h.to_lowercase().starts_with("host:"))
            .and_then(|h| h.split(':').nth(1))
            .map(|s| s.trim())
            .unwrap_or("localhost");
        format!("http://{}{}", host, target)
    };

    info!("HTTP proxy {} request to {}", method, url);

    // Check for Expect: 100-continue header and respond before reading body
    let expects_continue = headers.iter()
        .any(|h| h.to_lowercase().starts_with("expect:") && h.to_lowercase().contains("100-continue"));
    
    if expects_continue {
        write_half.write_all(b"HTTP/1.1 100 Continue\r\n\r\n").await?;
        write_half.flush().await?;
        debug!("  Sent 100 Continue");
    }

    // Check if this is an upstream registry URL and rewrite it
    let should_rewrite = url::Url::parse(&url).ok().map_or(false, |parsed| {
        parsed.host_str().map_or(false, |url_host| {
            upstream_hosts.iter().any(|upstream_host| {
                url_host == upstream_host || url_host.ends_with(&format!(".{}", upstream_host))
            })
        })
    });
    
    let final_url = if should_rewrite {
        // Rewrite to main proxy
        if let Ok(parsed) = url::Url::parse(&url) {
            let path = parsed.path();
            let query = parsed.query().map(|q| format!("?{}", q)).unwrap_or_default();
            let rewritten = format!("http://{}:{}{}{}", main_proxy_host, main_proxy_port, path, query);
            info!("  -> Rewriting to: {}", rewritten);
            rewritten
        } else {
            url.clone()
        }
    } else {
        url.clone()
    };

    // Get content length if present
    let content_length: usize = headers.iter()
        .find(|h| h.to_lowercase().starts_with("content-length:"))
        .and_then(|h| h.split(':').nth(1))
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);

    // Read body if present
    let body = if content_length > 0 {
        let mut body = vec![0u8; content_length];
        tokio::io::AsyncReadExt::read_exact(buf_reader, &mut body).await?;
        body
    } else {
        Vec::new()
    };

    let request = match method {
        "GET" => client.get(&final_url),
        "POST" => client.post(&final_url).body(body),
        "PUT" => client.put(&final_url).body(body),
        "DELETE" => client.delete(&final_url),
        "HEAD" => client.head(&final_url),
        _ => {
            write_half.write_all(b"HTTP/1.1 405 Method Not Allowed\r\nconnection: keep-alive\r\ncontent-length: 0\r\n\r\n").await?;
            return Ok(false);
        }
    };

    // Forward relevant headers
    let mut request = request;
    for header in headers {
        if let Some(colon_pos) = header.find(':') {
            let name = header[..colon_pos].trim();
            let value = header[colon_pos + 1..].trim();
            // Skip hop-by-hop headers and host (we're rewriting)
            if !["host", "connection", "proxy-connection", "proxy-authorization", "te", "trailer", "transfer-encoding", "upgrade", "expect"]
                .contains(&name.to_lowercase().as_str())
            {
                request = request.header(name, value);
            }
        }
    }

    match request.send().await {
        Ok(response) => {
            let status = response.status();
            let status_line = format!("HTTP/1.1 {} {}\r\n", status.as_u16(), status.canonical_reason().unwrap_or("OK"));
            write_half.write_all(status_line.as_bytes()).await?;

            // Write response headers
            for (key, value) in response.headers().iter() {
                if key != "transfer-encoding" && key != "connection" {
                    let header_line = format!("{}: {}\r\n", key, value.to_str().unwrap_or(""));
                    write_half.write_all(header_line.as_bytes()).await?;
                }
            }

            // Get body
            let body = response.bytes().await?;
            
            // Write content-length, connection keep-alive and body
            let cl_header = format!("content-length: {}\r\nconnection: keep-alive\r\n\r\n", body.len());
            write_half.write_all(cl_header.as_bytes()).await?;
            write_half.write_all(&body).await?;
            write_half.flush().await?;
        }
        Err(e) => {
            error!("HTTP proxy: upstream request failed: {}", e);
            write_half.write_all(b"HTTP/1.1 502 Bad Gateway\r\nconnection: keep-alive\r\ncontent-length: 0\r\n\r\n").await?;
            write_half.flush().await?;
        }
    }

    // Check for Connection: close
    let should_close = headers.iter()
        .any(|h| h.to_lowercase().starts_with("connection:") && h.to_lowercase().contains("close"));
    
    Ok(should_close)
}
