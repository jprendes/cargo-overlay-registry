use reqwest::Client;
use std::path::PathBuf;

/// Proxy state containing the HTTP client
pub struct ProxyState {
    pub client: Client,
    /// The base URL where this proxy is listening (for config.json rewriting)
    pub proxy_base_url: String,
    /// Local registry storage path
    pub local_registry_path: PathBuf,
    /// Upstream registry sparse index URL
    pub upstream_index: String,
    /// Upstream registry API URL
    pub upstream_api: String,
    /// Skip crates.io-style metadata validation on publish
    pub permissive_publishing: bool,
}

impl ProxyState {
    pub fn new(
        proxy_base_url: String,
        local_registry_path: PathBuf,
        upstream_index: String,
        upstream_api: String,
        permissive_publishing: bool,
    ) -> Self {
        Self {
            client: Client::builder()
                .user_agent("cargo-overlay-registry/0.1.0")
                .build()
                .expect("Failed to create HTTP client"),
            proxy_base_url,
            local_registry_path,
            upstream_index,
            upstream_api,
            permissive_publishing,
        }
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
