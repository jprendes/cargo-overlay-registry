use std::process::{Command, Stdio};
use std::time::Duration;
use std::thread;
use std::net::TcpStream;
use std::path::PathBuf;
use std::fs;

/// Wait for the server to be ready by attempting to connect
fn wait_for_server(host: &str, port: u16, timeout: Duration) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if TcpStream::connect(format!("{}:{}", host, port)).is_ok() {
            return true;
        }
        thread::sleep(Duration::from_millis(100));
    }
    false
}

/// Find an available port
fn find_available_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

#[test]
fn test_example_crate_build() {
    // Find project directories
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let example_dir = manifest_dir.join("example").join("hello-proxy");
    let target_dir = manifest_dir.join("target");
    
    // Build the proxy binary first
    let build_output = Command::new("cargo")
        .args(["build", "--release"])
        .current_dir(&manifest_dir)
        .output()
        .expect("Failed to build proxy");
    assert!(build_output.status.success(), "Failed to build proxy binary: {}", String::from_utf8_lossy(&build_output.stderr));

    let proxy_binary = target_dir.join("release").join("cargo-proxy-registry");
    assert!(proxy_binary.exists(), "Proxy binary not found at {:?}", proxy_binary);

    // Find available ports
    let port = find_available_port();
    let http_proxy_port = find_available_port();
    
    // Create a temporary registry directory for this test
    let test_registry = target_dir.join("test-registry");
    let _ = fs::remove_dir_all(&test_registry);
    fs::create_dir_all(&test_registry).expect("Failed to create test registry dir");

    // Create a temporary cargo home to avoid polluting the user's cache
    let test_cargo_home = target_dir.join("test-cargo-home");
    let _ = fs::remove_dir_all(&test_cargo_home);
    fs::create_dir_all(&test_cargo_home).expect("Failed to create test cargo home");

    // CA certificate path for MITM
    let ca_cert_path = target_dir.join("test-ca-cert.pem");

    // Start the proxy server with HTTP proxy
    let proxy_process = Command::new(&proxy_binary)
        .args([
            "--port", &port.to_string(),
            "--host", "127.0.0.1",
            "--registry-path", test_registry.to_str().unwrap(),
            "--http-proxy-port", &http_proxy_port.to_string(),
            "--ca-cert-out", ca_cert_path.to_str().unwrap(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to start proxy server");

    // Ensure we kill the proxy on test exit
    struct ProxyGuard(std::process::Child);
    impl Drop for ProxyGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let _guard = ProxyGuard(proxy_process);

    // Wait for servers to be ready
    assert!(
        wait_for_server("127.0.0.1", port, Duration::from_secs(10)),
        "Proxy server failed to start within timeout"
    );
    assert!(
        wait_for_server("127.0.0.1", http_proxy_port, Duration::from_secs(10)),
        "HTTP proxy failed to start within timeout"
    );

    // The workspace target directory is used for member builds
    let example_target = target_dir.clone();

    // Build the example crate using only the HTTP proxy
    let http_proxy_url = format!("http://127.0.0.1:{}", http_proxy_port);
    let build_output = Command::new("cargo")
        .args(["build"])
        .current_dir(&example_dir)
        .env("CARGO_HOME", &test_cargo_home)
        .env("CARGO_HTTP_PROXY", &http_proxy_url)
        .env("CARGO_HTTP_CAINFO", &ca_cert_path)
        .env("RUST_LOG", "debug")
        .output()
        .expect("Failed to run cargo build on example");

    let stdout = String::from_utf8_lossy(&build_output.stdout);
    let stderr = String::from_utf8_lossy(&build_output.stderr);
    
    println!("=== Example build stdout ===\n{}", stdout);
    println!("=== Example build stderr ===\n{}", stderr);

    assert!(
        build_output.status.success(),
        "Example crate build failed!\nstdout: {}\nstderr: {}",
        stdout,
        stderr
    );

    // Verify the example binary was created
    let example_binary = example_target.join("debug").join("hello-proxy");
    assert!(
        example_binary.exists(),
        "Example binary not found at {:?}",
        example_binary
    );

    // Run the example and verify output
    let run_output = Command::new(&example_binary)
        .output()
        .expect("Failed to run example binary");

    let run_stdout = String::from_utf8_lossy(&run_output.stdout);
    println!("=== Example run output ===\n{}", run_stdout);

    assert!(run_output.status.success(), "Example binary failed to run");
    assert!(
        run_stdout.contains("Hello, World!"),
        "Expected 'Hello, World!' in output, got: {}",
        run_stdout
    );

    println!("Integration test passed!");
}

#[test]
fn test_local_publish_and_download() {
    // Find project directories
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let quote_dir = manifest_dir.join("example").join("quote");
    let target_dir = manifest_dir.join("target");
    
    // Build the proxy binary first
    let build_output = Command::new("cargo")
        .args(["build", "--release"])
        .current_dir(&manifest_dir)
        .output()
        .expect("Failed to build proxy");
    assert!(build_output.status.success(), "Failed to build proxy binary: {}", String::from_utf8_lossy(&build_output.stderr));

    let proxy_binary = target_dir.join("release").join("cargo-proxy-registry");

    // Find available ports
    let port = find_available_port();
    let http_proxy_port = find_available_port();
    
    // Create a temporary registry directory for this test
    let test_registry = target_dir.join("test-registry-publish");
    let _ = fs::remove_dir_all(&test_registry);
    fs::create_dir_all(&test_registry).expect("Failed to create test registry dir");

    // Create a temporary cargo home to avoid caching issues
    let test_cargo_home = target_dir.join("test-cargo-home-publish");
    let _ = fs::remove_dir_all(&test_cargo_home);
    fs::create_dir_all(&test_cargo_home).expect("Failed to create test cargo home");

    // CA certificate path for MITM
    let ca_cert_path = target_dir.join("test-ca-cert-publish.pem");

    // Start the proxy server with HTTP proxy
    let proxy_process = Command::new(&proxy_binary)
        .args([
            "--port", &port.to_string(),
            "--host", "127.0.0.1",
            "--registry-path", test_registry.to_str().unwrap(),
            "--http-proxy-port", &http_proxy_port.to_string(),
            "--ca-cert-out", ca_cert_path.to_str().unwrap(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to start proxy server");

    struct ProxyGuard(std::process::Child);
    impl Drop for ProxyGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let _guard = ProxyGuard(proxy_process);

    // Wait for servers to be ready
    assert!(
        wait_for_server("127.0.0.1", port, Duration::from_secs(10)),
        "Proxy server failed to start within timeout"
    );
    assert!(
        wait_for_server("127.0.0.1", http_proxy_port, Duration::from_secs(10)),
        "HTTP proxy failed to start within timeout"
    );

    // Step 1: Publish the quote crate using cargo publish
    let http_proxy_url = format!("http://127.0.0.1:{}", http_proxy_port);
    let publish_output = Command::new("cargo")
        .args(["publish", "--allow-dirty", "--token", "dummy"])
        .current_dir(&quote_dir)
        .env("CARGO_HOME", &test_cargo_home)
        .env("CARGO_HTTP_PROXY", &http_proxy_url)
        .env("CARGO_HTTP_CAINFO", &ca_cert_path)
        .output()
        .expect("Failed to run cargo publish");

    let stdout = String::from_utf8_lossy(&publish_output.stdout);
    let stderr = String::from_utf8_lossy(&publish_output.stderr);
    println!("=== Publish stdout ===\n{}", stdout);
    println!("=== Publish stderr ===\n{}", stderr);

    assert!(
        publish_output.status.success(),
        "cargo publish failed:\nstdout: {}\nstderr: {}",
        stdout,
        stderr
    );

    // Step 2: Build the test-consumer crate that depends on the published quote crate
    let consumer_dir = manifest_dir.join("example").join("test-consumer");

    let build_output = Command::new("cargo")
        .args(["build"])
        .current_dir(&consumer_dir)
        .env("CARGO_HOME", &test_cargo_home)
        .env("CARGO_HTTP_PROXY", &http_proxy_url)
        .env("CARGO_HTTP_CAINFO", &ca_cert_path)
        .output()
        .expect("Failed to run cargo build");

    let stdout = String::from_utf8_lossy(&build_output.stdout);
    let stderr = String::from_utf8_lossy(&build_output.stderr);
    println!("=== Consumer build stdout ===\n{}", stdout);
    println!("=== Consumer build stderr ===\n{}", stderr);

    assert!(
        build_output.status.success(),
        "Consumer crate build failed:\nstdout: {}\nstderr: {}",
        stdout,
        stderr
    );

    // Step 3: Run the consumer binary and verify it works
    // Note: test-consumer is part of the example workspace, so its binary is in example/target
    let consumer_binary = manifest_dir.join("example").join("target").join("debug").join("test-consumer");
    
    assert!(
        consumer_binary.exists(),
        "Consumer binary not found at {:?}",
        consumer_binary
    );

    let run_output = Command::new(&consumer_binary)
        .output()
        .expect("Failed to run consumer binary");

    let run_stdout = String::from_utf8_lossy(&run_output.stdout);
    println!("=== Consumer run output ===\n{}", run_stdout);

    assert!(run_output.status.success(), "Consumer binary failed to run");
    assert!(
        run_stdout.contains("Steve Jobs"),
        "Expected quote output, got: {}",
        run_stdout
    );

    println!("Publish/download integration test passed!");
}

#[test]
fn test_publish_quote_crate() {
    // Find project directories
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let quote_dir = manifest_dir.join("example").join("quote");
    let target_dir = manifest_dir.join("target");
    
    // Build the proxy binary first
    let build_output = Command::new("cargo")
        .args(["build", "--release"])
        .current_dir(&manifest_dir)
        .output()
        .expect("Failed to build proxy");
    assert!(build_output.status.success(), "Failed to build proxy binary: {}", String::from_utf8_lossy(&build_output.stderr));

    let proxy_binary = target_dir.join("release").join("cargo-proxy-registry");

    // Find available ports
    let port = find_available_port();
    let http_proxy_port = find_available_port();
    
    // Create a temporary registry directory for this test
    let test_registry = target_dir.join("test-registry-quote");
    let _ = fs::remove_dir_all(&test_registry);
    fs::create_dir_all(&test_registry).expect("Failed to create test registry dir");

    // Create a temporary cargo home to avoid caching issues
    let test_cargo_home = target_dir.join("test-cargo-home-quote");
    let _ = fs::remove_dir_all(&test_cargo_home);
    fs::create_dir_all(&test_cargo_home).expect("Failed to create test cargo home");

    // CA certificate path for MITM
    let ca_cert_path = target_dir.join("test-ca-cert-quote.pem");

    // Start the proxy server with HTTP proxy
    let proxy_process = Command::new(&proxy_binary)
        .args([
            "--port", &port.to_string(),
            "--host", "127.0.0.1",
            "--registry-path", test_registry.to_str().unwrap(),
            "--http-proxy-port", &http_proxy_port.to_string(),
            "--ca-cert-out", ca_cert_path.to_str().unwrap(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to start proxy server");

    struct ProxyGuard(std::process::Child);
    impl Drop for ProxyGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let _guard = ProxyGuard(proxy_process);

    // Wait for servers to be ready
    assert!(
        wait_for_server("127.0.0.1", port, Duration::from_secs(10)),
        "Proxy server failed to start within timeout"
    );
    assert!(
        wait_for_server("127.0.0.1", http_proxy_port, Duration::from_secs(10)),
        "HTTP proxy failed to start within timeout"
    );

    // Publish using cargo publish with environment variables
    let http_proxy_url = format!("http://127.0.0.1:{}", http_proxy_port);
    let publish_output = Command::new("cargo")
        .args(["publish", "--allow-dirty", "--token", "dummy"])
        .current_dir(&quote_dir)
        .env("CARGO_HOME", &test_cargo_home)
        .env("CARGO_HTTP_PROXY", &http_proxy_url)
        .env("CARGO_HTTP_CAINFO", &ca_cert_path)
        .output()
        .expect("Failed to run cargo publish");

    let stdout = String::from_utf8_lossy(&publish_output.stdout);
    let stderr = String::from_utf8_lossy(&publish_output.stderr);
    
    println!("=== Publish stdout ===\n{}", stdout);
    println!("=== Publish stderr ===\n{}", stderr);

    assert!(
        publish_output.status.success(),
        "cargo publish failed:\nstdout: {}\nstderr: {}",
        stdout,
        stderr
    );

    // Verify the crate was stored
    let stored_crate = test_registry.join("crates").join("quote").join("99.0.0.crate");
    assert!(
        stored_crate.exists(),
        "Crate file not found in registry at {:?}",
        stored_crate
    );

    // Verify the index was updated
    let index_file = test_registry.join("index").join("qu").join("ot").join("quote");
    assert!(
        index_file.exists(),
        "Index file not found at {:?}",
        index_file
    );

    let index_content = fs::read_to_string(&index_file).expect("Failed to read index file");
    println!("Index content:\n{}", index_content);
    assert!(index_content.contains("\"name\":\"quote\""), "Index doesn't contain quote");
    assert!(index_content.contains("\"vers\":\"99.0.0\""), "Index doesn't contain version 99.0.0");

    println!("Quote crate publish test passed!");
}

#[test]
fn test_workspace_publish() {
    // Find project directories
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let example_dir = manifest_dir.join("example");
    let target_dir = manifest_dir.join("target");
    
    // Build the proxy binary first
    let build_output = Command::new("cargo")
        .args(["build", "--release"])
        .current_dir(&manifest_dir)
        .output()
        .expect("Failed to build proxy");
    assert!(build_output.status.success(), "Failed to build proxy binary: {}", String::from_utf8_lossy(&build_output.stderr));

    let proxy_binary = target_dir.join("release").join("cargo-proxy-registry");

    // Find available ports
    let port = find_available_port();
    let http_proxy_port = find_available_port();
    
    // Create a temporary registry directory for this test
    let test_registry = target_dir.join("test-registry-workspace");
    let _ = fs::remove_dir_all(&test_registry);
    fs::create_dir_all(&test_registry).expect("Failed to create test registry dir");

    // Create a temporary cargo home to avoid caching issues
    let test_cargo_home = target_dir.join("test-cargo-home-workspace");
    let _ = fs::remove_dir_all(&test_cargo_home);
    fs::create_dir_all(&test_cargo_home).expect("Failed to create test cargo home");

    // CA certificate path for MITM
    let ca_cert_path = target_dir.join("test-ca-cert-workspace.pem");

    // Start the proxy server with HTTP proxy
    let proxy_process = Command::new(&proxy_binary)
        .args([
            "--port", &port.to_string(),
            "--host", "127.0.0.1",
            "--registry-path", test_registry.to_str().unwrap(),
            "--http-proxy-port", &http_proxy_port.to_string(),
            "--ca-cert-out", ca_cert_path.to_str().unwrap(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to start proxy server");

    struct ProxyGuard(std::process::Child);
    impl Drop for ProxyGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let _guard = ProxyGuard(proxy_process);

    // Wait for servers to be ready
    assert!(
        wait_for_server("127.0.0.1", port, Duration::from_secs(10)),
        "Proxy server failed to start within timeout"
    );
    assert!(
        wait_for_server("127.0.0.1", http_proxy_port, Duration::from_secs(10)),
        "HTTP proxy failed to start within timeout"
    );

    // Publish the entire workspace using cargo publish --workspace
    let http_proxy_url = format!("http://127.0.0.1:{}", http_proxy_port);
    let publish_output = Command::new("cargo")
        .args(["publish", "--workspace", "--allow-dirty", "--token", "dummy"])
        .current_dir(&example_dir)
        .env("CARGO_HOME", &test_cargo_home)
        .env("CARGO_HTTP_PROXY", &http_proxy_url)
        .env("CARGO_HTTP_CAINFO", &ca_cert_path)
        .output()
        .expect("Failed to run cargo publish --workspace");

    let stdout = String::from_utf8_lossy(&publish_output.stdout);
    let stderr = String::from_utf8_lossy(&publish_output.stderr);
    
    println!("=== Workspace publish stdout ===\n{}", stdout);
    println!("=== Workspace publish stderr ===\n{}", stderr);

    assert!(
        publish_output.status.success(),
        "cargo publish --workspace failed:\nstdout: {}\nstderr: {}",
        stdout,
        stderr
    );

    // Verify quote was published
    let quote_crate = test_registry.join("crates").join("quote").join("99.0.0.crate");
    assert!(
        quote_crate.exists(),
        "quote crate file not found in registry at {:?}",
        quote_crate
    );

    // Verify hello-proxy was published
    let hello_crate = test_registry.join("crates").join("hello-proxy").join("0.1.0.crate");
    assert!(
        hello_crate.exists(),
        "hello-proxy crate file not found in registry at {:?}",
        hello_crate
    );

    // Verify test-consumer was published
    let consumer_crate = test_registry.join("crates").join("test-consumer").join("0.1.0.crate");
    assert!(
        consumer_crate.exists(),
        "test-consumer crate file not found in registry at {:?}",
        consumer_crate
    );

    // Verify index files were created
    let quote_index = test_registry.join("index").join("qu").join("ot").join("quote");
    assert!(quote_index.exists(), "quote index file not found");
    
    let hello_index = test_registry.join("index").join("he").join("ll").join("hello-proxy");
    assert!(hello_index.exists(), "hello-proxy index file not found");
    
    let consumer_index = test_registry.join("index").join("te").join("st").join("test-consumer");
    assert!(consumer_index.exists(), "test-consumer index file not found");

    // Verify test-consumer's index entry has quote as a dependency
    let consumer_index_content = fs::read_to_string(&consumer_index).expect("Failed to read consumer index");
    println!("test-consumer index content:\n{}", consumer_index_content);
    assert!(
        consumer_index_content.contains("\"name\":\"quote\"") || consumer_index_content.contains("quote"),
        "test-consumer index should reference quote dependency"
    );

    println!("Workspace publish test passed!");
}
