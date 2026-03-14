//! cargo-publish-dry-run - Run `cargo publish --dry-run` with a local overlay registry
//!
//! This binary starts a local overlay registry server and runs `cargo publish --dry-run`
//! with the correct environment variables configured to route traffic through the proxy.
//!
//! All arguments are forwarded to `cargo publish --dry-run`.

use std::net::TcpStream;
use std::process::{Command, ExitCode};
use std::sync::Arc;
use std::time::Duration;

use cargo_overlay_registry::{
    build_registry_router, handle_proxy_connection, GenericProxyState, HttpProxyState, MitmCa,
};
use tokio::fs;
use tokio::sync::oneshot;

/// Find an available port
fn find_available_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

/// Wait for the server to be ready
fn wait_for_server(port: u16, timeout: Duration) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if TcpStream::connect(format!("127.0.0.1:{}", port)).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

#[tokio::main]
async fn main() -> ExitCode {
    // Initialize logger
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();

    // Find available port
    let port = find_available_port();
    let host = "127.0.0.1";
    let base_url = "https://crates.io".to_string();

    // Create temporary directories
    let temp_dir = tempfile::tempdir().expect("Failed to create temporary directory");
    let temp_path = temp_dir.path();

    let registry_path = temp_path.join("registry");
    fs::create_dir_all(&registry_path)
        .await
        .expect("Failed to create registry directory");
    fs::create_dir_all(registry_path.join("crates"))
        .await
        .expect("Failed to create crates directory");
    fs::create_dir_all(registry_path.join("index"))
        .await
        .expect("Failed to create index directory");

    let ca_cert_path = temp_path.join("ca-cert.pem");

    // Use a target directory inside the temp folder so tmp-registry is in a known location
    let target_dir = temp_path.join("target");
    let tmp_registry = target_dir.join("package").join("tmp-registry");

    // Create proxy state with tmp-registry (PublishRegistry) layered on top
    let state = Arc::new(GenericProxyState::for_publish(
        base_url.clone(),
        registry_path.clone(),
        tmp_registry.clone(),
        "https://index.crates.io".to_string(),
        "https://crates.io".to_string(),
        false, // enforce crates.io-style metadata validation
    ));

    // Generate MITM CA certificate
    let mitm_ca = Arc::new(MitmCa::new().expect("Failed to generate MITM CA certificate"));

    // Export CA certificate
    std::fs::write(&ca_cert_path, mitm_ca.ca_cert_pem()).expect("Failed to write CA certificate");

    // Set up HTTP proxy state
    let upstream_hosts = Arc::new(vec![
        "index.crates.io".to_string(),
        "crates.io".to_string(),
        "static.crates.io".to_string(),
    ]);

    let http_proxy_state = HttpProxyState {
        proxy_state: state.clone(),
        mitm_ca,
        upstream_hosts,
    };

    // Build the router
    let app = build_registry_router(state);

    // Create shutdown channel
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();

    // Spawn the server (no TLS - the proxy accepts plain HTTP and handles MITM internally)
    let bind_addr = format!("{}:{}", host, port);
    let server_handle = tokio::spawn(async move {
        let listener = tokio::net::TcpListener::bind(&bind_addr)
            .await
            .expect("Failed to bind to port");

        loop {
            tokio::select! {
                result = listener.accept() => {
                    let (stream, _addr) = result.expect("Failed to accept");
                    let app = app.clone();
                    let proxy_state = http_proxy_state.clone();

                    tokio::spawn(handle_proxy_connection(
                        stream,
                        app,
                        proxy_state,
                        None, // No TLS - plain HTTP proxy
                    ));
                }
                _ = &mut shutdown_rx => {
                    break;
                }
            }
        }
    });

    // Wait for server to be ready
    if !wait_for_server(port, Duration::from_secs(10)) {
        eprintln!("Error: Server failed to start within timeout");
        return ExitCode::FAILURE;
    }

    // Build cargo publish command
    let mut cmd = Command::new("cargo");
    cmd.arg("publish");

    // Forward all command-line arguments to cargo publish
    // When invoked as `cargo publish-dry-run`, cargo passes "publish-dry-run" as first arg
    let args: Vec<String> = std::env::args().skip(1).collect();
    for (i, arg) in args.iter().enumerate() {
        // Skip the subcommand name only if it's the first argument
        // (when invoked via `cargo publish-dry-run`)
        if i == 0 && arg == "publish-dry-run" {
            continue;
        }
        cmd.arg(arg);
    }

    // Set environment variables for cargo
    let http_proxy_url = format!("http://127.0.0.1:{}", port);
    cmd.env("CARGO_HTTP_PROXY", &http_proxy_url)
        .env("CARGO_HTTP_CAINFO", &ca_cert_path)
        .env("CARGO_REGISTRY_TOKEN", "dummy-token")
        .env("CARGO_TARGET_DIR", &target_dir);

    // Run cargo publish
    let status = cmd.status().expect("Failed to run cargo publish");

    // Shutdown the server
    let _ = shutdown_tx.send(());
    server_handle.abort();

    if status.success() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(status.code().unwrap_or(1) as u8)
    }
}
