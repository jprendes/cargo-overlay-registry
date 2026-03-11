mod cli;
mod endpoints;
mod http_proxy;
mod registry;
mod state;
mod tls;
mod types;

use std::sync::Arc;

use axum::routing::{get, put};
use axum::Router;
use axum_server::tls_rustls::RustlsConfig;
use clap::Parser;
use cli::Args;
use endpoints::{
    handle_api_download, handle_api_publish, handle_api_search, handle_config, handle_index_1char,
    handle_index_2char, handle_index_3char, handle_index_4plus,
};
use http_proxy::run_http_proxy;
use log::info;
use state::{MitmCa, ProxyState};
use tls::generate_self_signed_cert;
use tokio::fs;

#[tokio::main]
async fn main() {
    // Initialize the logger
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args = Args::parse();

    // Determine protocol and base URL
    let use_tls = args.tls || args.tls_cert.is_some();
    let protocol = if use_tls { "https" } else { "http" };

    let proxy_base_url = args
        .base_url
        .unwrap_or_else(|| format!("{}://localhost:{}", protocol, args.port));

    let local_registry_path = args.registry_path;

    info!(
        "Starting cargo registry proxy on {}:{}",
        args.host, args.port
    );
    info!("Proxy base URL: {}", proxy_base_url);
    info!("Local registry path: {}", local_registry_path.display());
    info!("Proxying index from: {}", args.upstream_index);
    info!("Proxying API from: {}", args.upstream_api);
    if use_tls {
        info!("TLS enabled");
    }

    // Create local registry directories
    fs::create_dir_all(local_registry_path.join("crates"))
        .await
        .ok();
    fs::create_dir_all(local_registry_path.join("index"))
        .await
        .ok();

    // Extract hosts from upstream URLs for HTTP proxy interception (before args are moved)
    let upstream_hosts: Vec<String> = [&args.upstream_index, &args.upstream_api]
        .iter()
        .filter_map(|url_str| {
            url::Url::parse(url_str)
                .ok()
                .and_then(|u| u.host_str().map(|h| h.to_string()))
        })
        .collect();

    let state = Arc::new(ProxyState::new(
        proxy_base_url.clone(),
        local_registry_path,
        args.upstream_index,
        args.upstream_api,
        args.permissive_publishing,
    ));

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
            std::fs::write(ca_cert_path, mitm_ca.ca_cert_pem())
                .expect("Failed to write CA certificate");
            info!("Exported CA certificate to {:?}", ca_cert_path);
            info!(
                "Set CARGO_HTTP_CAINFO={:?} to trust the proxy's certificates",
                ca_cert_path
            );
        }

        info!("Starting HTTP proxy on {}", http_proxy_addr);
        info!(
            "Set CARGO_HTTP_PROXY=http://{} to route traffic through proxy",
            http_proxy_addr
        );

        tokio::spawn(async move {
            run_http_proxy(
                &http_proxy_addr,
                &main_proxy_host,
                main_proxy_port,
                mitm_ca,
                upstream_hosts,
            )
            .await;
        });
    }

    if use_tls {
        // Load or generate TLS configuration
        let tls_config = if let (Some(cert_path), Some(key_path)) = (&args.tls_cert, &args.tls_key)
        {
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
