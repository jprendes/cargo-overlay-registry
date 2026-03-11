use clap::Parser;
use std::path::PathBuf;

/// Cargo registry proxy - proxies crates.io and supports local publishing
#[derive(Parser, Debug)]
#[command(name = "cargo-overlay-registry")]
#[command(author, version, about, long_about = None)]
pub struct Args {
    /// Port to listen on
    #[arg(short, long, default_value = "8080")]
    pub port: u16,

    /// Host/IP to bind to
    #[arg(short = 'H', long, default_value = "0.0.0.0")]
    pub host: String,

    /// Base URL for the proxy (used in config.json)
    /// Defaults to http://localhost:<port>
    #[arg(short, long)]
    pub base_url: Option<String>,

    /// Path to store locally published crates
    #[arg(short, long, default_value = "./local-registry")]
    pub registry_path: PathBuf,

    /// Upstream registry sparse index URL
    #[arg(long, default_value = "https://index.crates.io")]
    pub upstream_index: String,

    /// Upstream registry API URL
    #[arg(long, default_value = "https://crates.io")]
    pub upstream_api: String,

    /// Optional HTTP proxy port (for CARGO_HTTP_PROXY)
    /// When set, starts an HTTP forward proxy that intercepts traffic
    #[arg(long)]
    pub http_proxy_port: Option<u16>,

    /// Path to export CA certificate (PEM format) for MITM interception
    /// Use with CARGO_HTTP_CAINFO to make cargo trust the proxy's certificates
    #[arg(long)]
    pub ca_cert_out: Option<PathBuf>,

    /// Path to TLS certificate file (PEM format)
    /// If not provided but --tls is set, a self-signed certificate will be generated
    #[arg(long)]
    pub tls_cert: Option<PathBuf>,

    /// Path to TLS private key file (PEM format)
    /// Required if --tls-cert is provided
    #[arg(long)]
    pub tls_key: Option<PathBuf>,

    /// Enable HTTPS with self-signed certificate (if --tls-cert not provided)
    #[arg(long)]
    pub tls: bool,
}
