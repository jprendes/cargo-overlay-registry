use std::path::PathBuf;

use cargo_overlay_registry::RegistrySpec;
use clap::Parser;

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
    #[arg(short, long, default_value = "https://crates.io")]
    pub base_url: String,

    /// Registry layers (top to bottom). Examples:
    ///   -r local              Local registry in temp dir
    ///   -r local=/path        Local registry at path
    ///   -r crates.io          Shortcut for crates.io remote
    ///   -r remote=https://my-registry.com
    ///   -r remote=https://api.com,https://index.com
    #[arg(short = 'r', long = "registry", value_name = "SPEC")]
    pub registries: Vec<RegistrySpec>,

    /// Disable proxy mode (CONNECT handling with MITM)
    /// By default, the server acts as a forward proxy for cargo (HTTP or HTTPS)
    #[arg(long)]
    pub no_proxy: bool,

    /// Make the registry read-only (reject all publish requests)
    #[arg(long)]
    pub read_only: bool,

    /// Path to export CA certificate (PEM format) for MITM interception
    /// Use with CARGO_HTTP_CAINFO to make cargo trust the proxy's certificates
    #[arg(long)]
    pub ca_cert_out: Option<PathBuf>,

    /// Path to TLS certificate file (PEM format)
    /// If not provided, a self-signed certificate will be generated
    #[arg(long)]
    pub tls_cert: Option<PathBuf>,

    /// Path to TLS private key file (PEM format)
    /// Required if --tls-cert is provided
    #[arg(long)]
    pub tls_key: Option<PathBuf>,

    /// Disable HTTPS (use plain HTTP instead)
    #[arg(long)]
    pub no_tls: bool,

    /// Skip crates.io-style metadata validation on publish
    /// (by default, description, license/license-file, valid keywords, etc. are required)
    #[arg(long)]
    pub permissive_publishing: bool,
}

impl Args {
    /// Get the effective registries, applying defaults if none specified
    pub fn effective_registries(&self) -> Vec<RegistrySpec> {
        if self.registries.is_empty() {
            vec![RegistrySpec::local_temp(), RegistrySpec::crates_io()]
        } else {
            self.registries.clone()
        }
    }
}
