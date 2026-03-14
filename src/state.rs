use std::path::PathBuf;

use reqwest::Client;

use crate::registry::{
    AnyRegistry, LocalRegistry, OverlayRegistry, PublishRegistry, Registry, RemoteRegistry,
};

/// Type alias for the overlay registry used by the proxy
pub type ProxyRegistry = OverlayRegistry<LocalRegistry, RemoteRegistry>;

/// Type alias for the publish registry (local > publish > remote)
pub type PublishProxyRegistry = OverlayRegistry<LocalRegistry, OverlayRegistry<PublishRegistry, RemoteRegistry>>;

/// Trait for proxy state types that can be used with the registry endpoints
pub trait RegistryState: Send + Sync {
    /// The registry type this state uses
    type Registry: Registry + Send + Sync;

    /// Get a reference to the registry
    fn registry(&self) -> &Self::Registry;

    /// Get the upstream API URL (for search proxying)
    fn upstream_api(&self) -> &str;

    /// Get the base URL where the proxy is listening
    fn proxy_base_url(&self) -> &str;

    /// Get the HTTP client
    fn client(&self) -> &Client;
}

/// A generic proxy state that uses type-erased registries.
///
/// This simplifies code that doesn't need to know the concrete registry type.
#[derive(Clone)]
pub struct GenericProxyState {
    /// HTTP client for API proxying (search, etc.)
    client: Client,
    /// The base URL where this proxy is listening (for config.json rewriting)
    proxy_base_url: String,
    /// The upstream API URL (for search proxying)
    upstream_api: String,
    /// The type-erased registry
    registry: AnyRegistry,
}

impl GenericProxyState {
    /// Create a new generic proxy state from any registry.
    pub fn new(
        proxy_base_url: String,
        upstream_api: String,
        registry: impl Registry + 'static,
    ) -> Self {
        Self {
            client: Client::builder()
                .user_agent("cargo-overlay-registry/0.1.0")
                .build()
                .expect("Failed to create HTTP client"),
            proxy_base_url,
            upstream_api,
            registry: AnyRegistry::new(registry),
        }
    }

    /// Create a standard proxy state (local overlay on remote).
    pub fn standard(
        proxy_base_url: String,
        local_registry_path: PathBuf,
        upstream_index: String,
        upstream_api: String,
        permissive_publishing: bool,
    ) -> Self {
        let local = LocalRegistry::new(local_registry_path, !permissive_publishing);
        let remote = RemoteRegistry::new(upstream_index, upstream_api.clone());
        let registry = OverlayRegistry::new(local, remote);
        Self::new(proxy_base_url, upstream_api, registry)
    }

    /// Create a publish-dry-run proxy state (local > publish > remote).
    pub fn for_publish(
        proxy_base_url: String,
        local_registry_path: PathBuf,
        publish_registry_path: PathBuf,
        upstream_index: String,
        upstream_api: String,
        permissive_publishing: bool,
    ) -> Self {
        let remote = RemoteRegistry::new(upstream_index, upstream_api.clone());
        let publish = PublishRegistry::new(publish_registry_path);
        let publish_over_remote = OverlayRegistry::new(publish, remote);

        let local = LocalRegistry::new(local_registry_path, !permissive_publishing);
        let registry = OverlayRegistry::new(local, publish_over_remote);
        Self::new(proxy_base_url, upstream_api, registry)
    }
}

impl RegistryState for GenericProxyState {
    type Registry = AnyRegistry;

    fn registry(&self) -> &Self::Registry {
        &self.registry
    }

    fn upstream_api(&self) -> &str {
        &self.upstream_api
    }

    fn proxy_base_url(&self) -> &str {
        &self.proxy_base_url
    }

    fn client(&self) -> &Client {
        &self.client
    }
}

/// Proxy state containing the registry and HTTP client
#[derive(Clone)]
pub struct ProxyState {
    /// HTTP client for API proxying (search, etc.)
    pub client: Client,
    /// The base URL where this proxy is listening (for config.json rewriting)
    pub proxy_base_url: String,
    /// The overlay registry (local on top of remote)
    pub registry: ProxyRegistry,
}

impl ProxyState {
    pub fn new(
        proxy_base_url: String,
        local_registry_path: PathBuf,
        upstream_index: String,
        upstream_api: String,
        permissive_publishing: bool,
    ) -> Self {
        let local = LocalRegistry::new(local_registry_path, !permissive_publishing);
        let remote = RemoteRegistry::new(upstream_index, upstream_api.clone());
        let registry = OverlayRegistry::new(local, remote);

        Self {
            client: Client::builder()
                .user_agent("cargo-overlay-registry/0.1.0")
                .build()
                .expect("Failed to create HTTP client"),
            proxy_base_url,
            registry,
        }
    }

    /// Get the upstream API URL (for search proxying)
    pub fn upstream_api(&self) -> &str {
        &self.registry.bottom.api_url
    }
}

impl RegistryState for ProxyState {
    type Registry = ProxyRegistry;

    fn registry(&self) -> &Self::Registry {
        &self.registry
    }

    fn upstream_api(&self) -> &str {
        &self.registry.bottom.api_url
    }

    fn proxy_base_url(&self) -> &str {
        &self.proxy_base_url
    }

    fn client(&self) -> &Client {
        &self.client
    }
}

/// Proxy state for publish-dry-run that includes cargo's tmp-registry
#[derive(Clone)]
pub struct PublishProxyState {
    /// HTTP client for API proxying (search, etc.)
    pub client: Client,
    /// The base URL where this proxy is listening (for config.json rewriting)
    pub proxy_base_url: String,
    /// The overlay registry (local > publish > remote)
    pub registry: PublishProxyRegistry,
}

impl PublishProxyState {
    pub fn new(
        proxy_base_url: String,
        local_registry_path: PathBuf,
        publish_registry_path: PathBuf,
        upstream_index: String,
        upstream_api: String,
        permissive_publishing: bool,
    ) -> Self {
        let remote = RemoteRegistry::new(upstream_index, upstream_api.clone());
        let publish = PublishRegistry::new(publish_registry_path);
        let publish_over_remote = OverlayRegistry::new(publish, remote);

        let local = LocalRegistry::new(local_registry_path, !permissive_publishing);
        let registry = OverlayRegistry::new(local, publish_over_remote);

        Self {
            client: Client::builder()
                .user_agent("cargo-overlay-registry/0.1.0")
                .build()
                .expect("Failed to create HTTP client"),
            proxy_base_url,
            registry,
        }
    }

    /// Get the upstream API URL (for search proxying)
    pub fn upstream_api(&self) -> &str {
        &self.registry.bottom.bottom.api_url
    }
}

impl RegistryState for PublishProxyState {
    type Registry = PublishProxyRegistry;

    fn registry(&self) -> &Self::Registry {
        &self.registry
    }

    fn upstream_api(&self) -> &str {
        &self.registry.bottom.bottom.api_url
    }

    fn proxy_base_url(&self) -> &str {
        &self.proxy_base_url
    }

    fn client(&self) -> &Client {
        &self.client
    }
}

/// CA certificate for MITM TLS interception
pub struct MitmCa {
    /// CA certificate in PEM format
    ca_cert_pem: Vec<u8>,
    /// CA key pair for signing domain certificates
    ca_key_pair: rcgen::KeyPair,
    /// CA certificate for signing
    ca_cert: rcgen::Certificate,
}

impl MitmCa {
    /// Generate a new CA certificate
    pub fn new() -> Result<Self, rcgen::Error> {
        use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, KeyPair, KeyUsagePurpose};

        let mut params = CertificateParams::default();
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        params
            .distinguished_name
            .push(DnType::CommonName, "Cargo Overlay Registry CA");
        params
            .distinguished_name
            .push(DnType::OrganizationName, "Cargo Overlay Registry");

        let key_pair = KeyPair::generate()?;
        let ca_cert = params.self_signed(&key_pair)?;

        let ca_cert_pem = ca_cert.pem().into_bytes();

        Ok(Self {
            ca_cert_pem,
            ca_key_pair: key_pair,
            ca_cert,
        })
    }

    /// Get the CA certificate in PEM format
    pub fn ca_cert_pem(&self) -> &[u8] {
        &self.ca_cert_pem
    }

    /// Generate a certificate for a domain, signed by this CA
    pub fn sign_domain_cert(&self, domain: &str) -> Result<(Vec<u8>, Vec<u8>), rcgen::Error> {
        use rcgen::{CertificateParams, DnType, KeyPair, SanType};

        let mut params = CertificateParams::default();
        params.distinguished_name.push(DnType::CommonName, domain);
        params.subject_alt_names = vec![SanType::DnsName(
            domain
                .try_into()
                .map_err(|_| rcgen::Error::CouldNotParseCertificate)?,
        )];

        // Add wildcard if domain has subdomains potential
        if !domain.starts_with("*.")
            && let Ok(wildcard) = format!("*.{}", domain).try_into()
        {
            params.subject_alt_names.push(SanType::DnsName(wildcard));
        }

        let key_pair = KeyPair::generate()?;
        let cert = params.signed_by(&key_pair, &self.ca_cert, &self.ca_key_pair)?;

        let cert_pem = cert.pem().into_bytes();
        let key_pem = key_pair.serialize_pem().into_bytes();

        Ok((cert_pem, key_pem))
    }
}
