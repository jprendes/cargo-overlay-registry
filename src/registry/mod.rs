mod local;
mod overlay;
mod remote;

pub use local::LocalRegistry;
pub use overlay::OverlayRegistry;
pub use remote::RemoteRegistry;

use crate::types::{IndexEntry, PublishMetadata};
use std::fmt;

/// Error type for registry operations
#[derive(Debug)]
pub enum RegistryError {
    /// Crate or version not found
    NotFound,
    /// Invalid metadata or request
    InvalidRequest(String),
    /// Storage I/O error
    Storage(std::io::Error),
    /// Serialization error
    Serialization(serde_json::Error),
    /// Validation failed
    ValidationFailed(Vec<String>),
    /// Network/HTTP error
    Network(String),
    /// Operation not supported
    NotSupported,
}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RegistryError::NotFound => write!(f, "not found"),
            RegistryError::InvalidRequest(msg) => write!(f, "invalid request: {}", msg),
            RegistryError::Storage(e) => write!(f, "storage error: {}", e),
            RegistryError::Serialization(e) => write!(f, "serialization error: {}", e),
            RegistryError::ValidationFailed(errors) => {
                write!(f, "validation failed: {}", errors.join("; "))
            }
            RegistryError::Network(msg) => write!(f, "network error: {}", msg),
            RegistryError::NotSupported => write!(f, "operation not supported"),
        }
    }
}

impl std::error::Error for RegistryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RegistryError::Storage(e) => Some(e),
            RegistryError::Serialization(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for RegistryError {
    fn from(e: std::io::Error) -> Self {
        RegistryError::Storage(e)
    }
}

impl From<serde_json::Error> for RegistryError {
    fn from(e: serde_json::Error) -> Self {
        RegistryError::Serialization(e)
    }
}

/// Trait for cargo registry operations
pub trait Registry: Send + Sync {
    /// Look up all index entries for a crate
    fn lookup(
        &self,
        crate_name: &str,
    ) -> impl std::future::Future<Output = Result<Vec<IndexEntry>, RegistryError>> + Send;

    /// Download a crate file (.crate)
    fn download(
        &self,
        crate_name: &str,
        version: &str,
    ) -> impl std::future::Future<Output = Result<Vec<u8>, RegistryError>> + Send;

    /// Publish a crate (store crate file and update index)
    fn publish(
        &self,
        metadata: PublishMetadata,
        crate_data: &[u8],
    ) -> impl std::future::Future<Output = Result<String, RegistryError>> + Send;
}

