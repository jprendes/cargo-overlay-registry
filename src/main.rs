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
use http_proxy::{handle_proxy_request, is_proxy_request, HttpProxyState};
use log::info;
use state::{MitmCa, ProxyState};
use tls::generate_self_signed_cert;
use tokio::fs;

#[tokio::main]
async fn main() {
    // Initialize the logger
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args = Args::parse();

    // Determine if TLS is enabled
    let use_tls = args.tls || args.tls_cert.is_some();

    // Determine if HTTP proxy mode is enabled (enabled by default unless --no-proxy)
    let enable_http_proxy = !args.no_proxy;

    let proxy_base_url = args.base_url.clone();

    // Use provided registry path or create a temporary directory
    let (_temp_dir, local_registry_path): (Option<tempfile::TempDir>, std::path::PathBuf) =
        if let Some(path) = args.registry_path {
            (None, path)
        } else {
            let temp_dir = tempfile::tempdir().expect("Failed to create temporary directory");
            let path = temp_dir.path().to_path_buf();
            (Some(temp_dir), path)
        };

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
    if enable_http_proxy {
        info!("HTTP proxy mode enabled");
    }

    // Create local registry directories
    fs::create_dir_all(local_registry_path.join("crates"))
        .await
        .ok();
    fs::create_dir_all(local_registry_path.join("index"))
        .await
        .ok();

    // Extract hosts from upstream URLs for HTTP proxy interception
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

    // Set up HTTP proxy state if enabled
    let http_proxy_state = if enable_http_proxy {
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

        let protocol = if use_tls { "https" } else { "http" };
        info!(
            "Set CARGO_HTTP_PROXY={}://{}:{} to route traffic through proxy",
            protocol, args.host, args.port
        );

        Some(HttpProxyState {
            proxy_state: state.clone(),
            mitm_ca,
            upstream_hosts: Arc::new(upstream_hosts),
        })
    } else {
        None
    };

    let bind_addr = format!("{}:{}", args.host, args.port);

    info!("Listening on {}", bind_addr);
    info!("Configure cargo to use: sparse+{}/", proxy_base_url);

    // Build the router with optional HTTP proxy fallback
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

    // Server startup with different modes:
    // - TLS + proxy: Custom service over TLS for CONNECT support
    // - TLS only: Simple axum_server TLS
    // - HTTP + proxy: Custom service over plain TCP
    // - HTTP only: Simple axum::serve

    if let Some(proxy_state) = http_proxy_state {
        // Proxy mode - use custom service to handle CONNECT and absolute URLs
        use axum::body::Body;
        use axum::extract::Request;
        use axum::response::IntoResponse;
        use hyper::service::service_fn;
        use hyper_util::rt::{TokioExecutor, TokioIo};
        use hyper_util::server::conn::auto::Builder;
        use std::convert::Infallible;
        use tower::Service;

        let listener = tokio::net::TcpListener::bind(&bind_addr)
            .await
            .expect("Failed to bind to port");

        if use_tls {
            // TLS with proxy support
            let (cert_pem, key_pem) = if let (Some(cert_path), Some(key_path)) =
                (&args.tls_cert, &args.tls_key)
            {
                info!("Loading TLS certificate from {:?}", cert_path);
                info!("Loading TLS key from {:?}", key_path);
                let cert_pem = std::fs::read(cert_path).expect("Failed to read TLS certificate");
                let key_pem = std::fs::read(key_path).expect("Failed to read TLS key");
                (cert_pem, key_pem)
            } else {
                info!("Generating self-signed TLS certificate");
                generate_self_signed_cert(&args.host)
                    .expect("Failed to generate self-signed certificate")
            };

            let certs = rustls_pemfile::certs(&mut cert_pem.as_slice())
                .collect::<Result<Vec<_>, _>>()
                .expect("Failed to parse TLS certificate");
            let key = rustls_pemfile::private_key(&mut key_pem.as_slice())
                .expect("Failed to parse TLS key")
                .expect("No private key found");

            let server_config = rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(certs, key)
                .expect("Failed to create TLS config");

            let tls_acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server_config));

            loop {
                let (stream, _addr) = listener.accept().await.expect("Failed to accept");
                let app = app.clone();
                let proxy_state = proxy_state.clone();
                let tls_acceptor = tls_acceptor.clone();

                tokio::spawn(async move {
                    let tls_stream = match tls_acceptor.accept(stream).await {
                        Ok(s) => s,
                        Err(e) => {
                            log::debug!("TLS handshake error: {}", e);
                            return;
                        }
                    };

                    let service = service_fn(move |request: Request<hyper::body::Incoming>| {
                        let mut app = app.clone();
                        let proxy_state = proxy_state.clone();

                        async move {
                            let (parts, body) = request.into_parts();
                            let body = Body::new(body);
                            let request = Request::from_parts(parts, body);

                            if is_proxy_request(&request) {
                                let response = handle_proxy_request(
                                    axum::extract::State(proxy_state),
                                    request,
                                )
                                .await;
                                Ok::<_, Infallible>(response)
                            } else {
                                let response = app.call(request).await.into_response();
                                Ok::<_, Infallible>(response)
                            }
                        }
                    });

                    let io = TokioIo::new(tls_stream);
                    if let Err(e) = Builder::new(TokioExecutor::new())
                        .serve_connection_with_upgrades(io, service)
                        .await
                    {
                        log::debug!("Connection error: {}", e);
                    }
                });
            }
        } else {
            // HTTP with proxy support
            loop {
                let (stream, _addr) = listener.accept().await.expect("Failed to accept");
                let app = app.clone();
                let proxy_state = proxy_state.clone();

                tokio::spawn(async move {
                    let service = service_fn(move |request: Request<hyper::body::Incoming>| {
                        let mut app = app.clone();
                        let proxy_state = proxy_state.clone();

                        async move {
                            let (parts, body) = request.into_parts();
                            let body = Body::new(body);
                            let request = Request::from_parts(parts, body);

                            if is_proxy_request(&request) {
                                let response = handle_proxy_request(
                                    axum::extract::State(proxy_state),
                                    request,
                                )
                                .await;
                                Ok::<_, Infallible>(response)
                            } else {
                                let response = app.call(request).await.into_response();
                                Ok::<_, Infallible>(response)
                            }
                        }
                    });

                    let io = TokioIo::new(stream);
                    if let Err(e) = Builder::new(TokioExecutor::new())
                        .serve_connection_with_upgrades(io, service)
                        .await
                    {
                        log::debug!("Connection error: {}", e);
                    }
                });
            }
        }
    } else if use_tls {
        // TLS without proxy support - use simple axum_server
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
        // Simple HTTP mode without proxy support
        let listener = tokio::net::TcpListener::bind(&bind_addr)
            .await
            .expect("Failed to bind to port");
        axum::serve(listener, app).await.expect("Server error");
    }
}
