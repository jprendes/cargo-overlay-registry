use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::thread;
use std::time::Duration;

use tempfile::TempDir;

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

/// Build the proxy binary (only once per test run)
fn build_proxy_binary() -> PathBuf {
    static PROXY_BINARY: OnceLock<PathBuf> = OnceLock::new();
    PROXY_BINARY
        .get_or_init(|| {
            let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            let target_dir = manifest_dir.join("target");

            let build_output = Command::new("cargo")
                .args(["build", "--release"])
                .current_dir(&manifest_dir)
                .output()
                .expect("Failed to build proxy");

            assert!(
                build_output.status.success(),
                "Failed to build proxy binary: {}",
                String::from_utf8_lossy(&build_output.stderr)
            );

            let proxy_binary = target_dir.join("release").join("cargo-overlay-registry");
            assert!(
                proxy_binary.exists(),
                "Proxy binary not found at {:?}",
                proxy_binary
            );

            proxy_binary
        })
        .clone()
}

/// A test helper that starts the proxy and provides configured cargo commands.
///
/// The proxy is automatically stopped when the guard is dropped.
pub struct ProxyTestHelper {
    process: Child,
    port: u16,
    http_proxy_port: u16,
    ca_cert_path: PathBuf,
    cargo_home: PathBuf,
    #[allow(dead_code)]
    pub registry_path: PathBuf,
    _temp_dir: TempDir,
}

impl ProxyTestHelper {
    /// Create a new proxy test helper with the given test name (used for temp directories).
    pub fn new(test_name: &str) -> Self {
        Self::with_args(test_name, &[])
    }

    /// Create a new proxy test helper with additional command-line arguments.
    pub fn with_args(test_name: &str, extra_args: &[&str]) -> Self {
        let proxy_binary = build_proxy_binary();

        // Find available ports
        let port = find_available_port();
        let http_proxy_port = find_available_port();

        // Create temporary directory for test artifacts
        let temp_dir = tempfile::tempdir().expect("Failed to create temp directory");
        let temp_path = temp_dir.path();

        let registry_path = temp_path.join("registry");
        std::fs::create_dir_all(&registry_path).expect("Failed to create registry dir");

        let cargo_home = temp_path.join("cargo-home");
        std::fs::create_dir_all(&cargo_home).expect("Failed to create cargo home");

        let ca_cert_path = temp_path.join(format!("{}-ca-cert.pem", test_name));

        // Build args
        let mut args = vec![
            "--port",
            &port.to_string(),
            "--host",
            "127.0.0.1",
            "--registry-path",
            registry_path.to_str().unwrap(),
            "--http-proxy-port",
            &http_proxy_port.to_string(),
            "--ca-cert-out",
            ca_cert_path.to_str().unwrap(),
        ]
        .into_iter()
        .map(String::from)
        .collect::<Vec<_>>();

        for arg in extra_args {
            args.push(arg.to_string());
        }

        // Start the proxy server
        let process = Command::new(&proxy_binary)
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("Failed to start proxy server");

        let helper = Self {
            process,
            port,
            http_proxy_port,
            ca_cert_path,
            cargo_home,
            registry_path,
            _temp_dir: temp_dir,
        };

        // Wait for servers to be ready
        assert!(
            wait_for_server("127.0.0.1", helper.port, Duration::from_secs(10)),
            "Proxy server failed to start within timeout"
        );
        assert!(
            wait_for_server("127.0.0.1", helper.http_proxy_port, Duration::from_secs(10)),
            "HTTP proxy failed to start within timeout"
        );

        helper
    }

    /// Returns a `Command` for running cargo with all proxy configuration set.
    pub fn cargo_command(&self) -> Command {
        let http_proxy_url = format!("http://127.0.0.1:{}", self.http_proxy_port);
        let mut cmd = Command::new("cargo");
        cmd.env("CARGO_HOME", &self.cargo_home)
            .env("CARGO_HTTP_PROXY", &http_proxy_url)
            .env("CARGO_HTTP_CAINFO", &self.ca_cert_path);
        cmd
    }
}

impl Drop for ProxyTestHelper {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}
