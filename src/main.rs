mod cli;

use std::sync::Arc;

use axum_server::tls_rustls::RustlsConfig;
use cargo_overlay_registry::{
    build_registry, build_registry_router, generate_self_signed_cert, handle_proxy_connection,
    GenericProxyState, HttpProxyState, MitmCa, RegistryBuildOptions, RegistrySpec,
};
use clap::Parser;
use cli::Args;
use log::info;

#[tokio::main]
async fn main() {
    // Initialize the logger
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();

    let args = Args::parse();

    // Determine if TLS is enabled (enabled by default unless --no-tls or --exec is used)
    // When executing a command, we use plain HTTP to avoid needing to trust the proxy's cert
    let use_tls = !args.no_tls && args.exec.is_none();

    // Determine if HTTP proxy mode is enabled (enabled by default unless --no-proxy)
    let enable_http_proxy = !args.no_proxy;

    let proxy_base_url = args.base_url.clone();

    // Get effective registries
    let registries = args.effective_registries();

    info!(
        "Starting cargo registry proxy on {}:{}",
        args.host, args.port
    );
    info!("Proxy base URL: {}", proxy_base_url);
    if use_tls {
        info!("TLS enabled");
    }
    if enable_http_proxy {
        info!("HTTP proxy mode enabled");
    }

    // Build registry overlay
    let options = RegistryBuildOptions {
        permissive_publishing: args.permissive_publishing,
        read_only: args.read_only,
    };
    let built = build_registry(&registries, &options);

    // Log registry info
    for (idx, spec) in registries.iter().enumerate() {
        match spec {
            RegistrySpec::Local { path } => {
                let is_writable = idx == 0 && !args.read_only;
                let mode = if is_writable { "writable" } else { "read-only" };
                if let Some(p) = path {
                    info!("Local registry ({}) at: {}", mode, p.display());
                } else {
                    info!("Local registry ({}) in temp dir", mode);
                }
            }
            RegistrySpec::Remote { api_url, index_url } => {
                info!("Remote registry: api={}, index={}", api_url, index_url);
            }
        }
    }

    let upstream_api = built.upstream_api(&registries);

    // Keep temp_dirs alive by moving them into _temp_dirs
    let _temp_dirs = built.temp_dirs;

    let state = Arc::new(GenericProxyState::new(
        proxy_base_url.clone(),
        upstream_api,
        built.registry,
    ));

    // Set up HTTP proxy state if enabled
    let (http_proxy_state, cargo_env) = if enable_http_proxy {
        info!("Intercepting hosts: {:?}", built.upstream_hosts);

        // Generate MITM CA certificate
        let mitm_ca = Arc::new(MitmCa::new().expect("Failed to generate MITM CA certificate"));

        // Export CA certificate (to specified path or temp file)
        let ca_cert_path = args.ca_cert_out.clone().unwrap_or_else(|| {
            let temp_dir = std::env::temp_dir();
            temp_dir.join("cargo-overlay-registry-ca.pem")
        });
        std::fs::write(&ca_cert_path, mitm_ca.ca_cert_pem())
            .expect("Failed to write CA certificate");
        info!("Exported CA certificate to {:?}", ca_cert_path);

        let protocol = if use_tls { "https" } else { "http" };
        // Use 127.0.0.1 for client connections when bound to wildcard addresses
        let connect_host = match args.host.as_str() {
            "0.0.0.0" => "127.0.0.1",
            "::" => "::1",
            h => h,
        };
        let proxy_url = format!("{}://{}:{}/", protocol, connect_host, args.port);
        // If user provides their own TLS cert, they should use that for CAINFO
        let cainfo_path = args.tls_cert.as_ref().unwrap_or(&ca_cert_path).clone();

        info!(
            "Set CARGO_HTTP_PROXY={} to route traffic through proxy",
            proxy_url
        );
        info!(
            "Set CARGO_HTTP_CAINFO={:?} to trust the proxy's certificates",
            cainfo_path
        );
        info!("Set CARGO_REGISTRY_TOKEN=dummy to enable publishing");

        // Only print env vars to stdout if not executing a command
        if args.exec.is_none() {
            println!("CARGO_HTTP_PROXY={}", proxy_url);
            println!("CARGO_HTTP_CAINFO={:?}", cainfo_path);
            println!("CARGO_REGISTRY_TOKEN=dummy");
        }

        let cargo_env = vec![
            ("CARGO_HTTP_PROXY".to_string(), proxy_url),
            ("CARGO_HTTP_CAINFO".to_string(), cainfo_path.to_string_lossy().to_string()),
            ("CARGO_REGISTRY_TOKEN".to_string(), "dummy".to_string()),
        ];

        (Some(HttpProxyState {
            proxy_state: state.clone(),
            mitm_ca,
            upstream_hosts: Arc::new(built.upstream_hosts),
        }), cargo_env)
    } else {
        (None, vec![])
    };

    let bind_addr = format!("{}:{}", args.host, args.port);

    info!("Listening on {}", bind_addr);
    info!("Configure cargo to use: sparse+{}/", proxy_base_url);

    // Build the router
    let app = build_registry_router(state);

    // If --exec is provided, run the command after the server starts
    if let Some(exec_args) = args.exec {
        if exec_args.is_empty() {
            eprintln!("Error: --exec requires a command");
            std::process::exit(1);
        }

        // Start the server in the background
        let server_handle = tokio::spawn(run_server(
            bind_addr.clone(),
            app,
            http_proxy_state,
            use_tls,
            args.tls_cert.clone(),
            args.tls_key.clone(),
            args.host.clone(),
        ));

        // Wait for the server to be ready
        for _ in 0..50 {
            if tokio::net::TcpStream::connect(&bind_addr).await.is_ok() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        // Run the command with cargo env vars set
        let status = std::process::Command::new(&exec_args[0])
            .args(&exec_args[1..])
            .envs(cargo_env)
            .status()
            .expect("Failed to execute command");

        // Abort the server
        server_handle.abort();

        // Exit with the same exit code
        std::process::exit(status.code().unwrap_or(1));
    }

    // Server startup with different modes:
    // - TLS + proxy: Custom service over TLS for CONNECT support
    // - TLS only: Simple axum_server TLS
    // - HTTP + proxy: Custom service over plain TCP
    // - HTTP only: Simple axum::serve

    run_server(
        bind_addr,
        app,
        http_proxy_state,
        use_tls,
        args.tls_cert,
        args.tls_key,
        args.host,
    )
    .await;
}

async fn run_server(
    bind_addr: String,
    app: axum::Router,
    http_proxy_state: Option<HttpProxyState>,
    use_tls: bool,
    tls_cert: Option<std::path::PathBuf>,
    tls_key: Option<std::path::PathBuf>,
    host: String,
) {
    if let Some(proxy_state) = http_proxy_state {
        // Proxy mode - use custom service to handle CONNECT and absolute URLs
        let listener = tokio::net::TcpListener::bind(&bind_addr)
            .await
            .expect("Failed to bind to port");

        let tls_acceptor = if use_tls {
            // TLS with proxy support
            let (cert_pem, key_pem) = if let (Some(cert_path), Some(key_path)) =
                (&tls_cert, &tls_key)
            {
                info!("Loading TLS certificate from {:?}", cert_path);
                info!("Loading TLS key from {:?}", key_path);
                let cert_pem = std::fs::read(cert_path).expect("Failed to read TLS certificate");
                let key_pem = std::fs::read(key_path).expect("Failed to read TLS key");
                (cert_pem, key_pem)
            } else {
                info!("Generating self-signed TLS certificate");
                generate_self_signed_cert(&host)
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

            Some(tokio_rustls::TlsAcceptor::from(Arc::new(server_config)))
        } else {
            None
        };

        loop {
            let (stream, _addr) = listener.accept().await.expect("Failed to accept");
            let app = app.clone();
            let proxy_state = proxy_state.clone();
            let tls_acceptor = tls_acceptor.clone();

            tokio::spawn(handle_proxy_connection(
                stream,
                app,
                proxy_state,
                tls_acceptor,
            ));
        }
    } else if use_tls {
        // TLS without proxy support - use simple axum_server
        let tls_config = if let (Some(cert_path), Some(key_path)) = (&tls_cert, &tls_key)
        {
            info!("Loading TLS certificate from {:?}", cert_path);
            info!("Loading TLS key from {:?}", key_path);
            RustlsConfig::from_pem_file(cert_path, key_path)
                .await
                .expect("Failed to load TLS certificate/key")
        } else {
            info!("Generating self-signed TLS certificate");
            let (cert_pem, key_pem) = generate_self_signed_cert(&host)
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
