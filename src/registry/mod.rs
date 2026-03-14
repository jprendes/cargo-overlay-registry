mod local;
mod overlay;
mod publish;
mod remote;

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

pub use local::LocalRegistry;
pub use overlay::OverlayRegistry;
pub use publish::PublishRegistry;
pub use remote::RemoteRegistry;

use crate::types::{IndexEntry, PublishMetadata};

/// Error type for registry operations
#[derive(Debug)]
pub enum RegistryError {
    /// Crate or version not found
    NotFound,
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

/// A dyn-compatible version of the Registry trait using boxed futures.
///
/// This trait mirrors `Registry` but returns boxed futures instead of impl Trait,
/// making it usable as a trait object (`dyn DynRegistry`).
pub trait DynRegistry: Send + Sync {
    /// Look up all index entries for a crate
    fn lookup<'a>(
        &'a self,
        crate_name: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<IndexEntry>, RegistryError>> + Send + 'a>>;

    /// Download a crate file (.crate)
    fn download<'a>(
        &'a self,
        crate_name: &'a str,
        version: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, RegistryError>> + Send + 'a>>;

    /// Publish a crate (store crate file and update index)
    fn publish<'a>(
        &'a self,
        metadata: PublishMetadata,
        crate_data: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = Result<String, RegistryError>> + Send + 'a>>;
}

/// Blanket implementation: any type implementing Registry automatically implements DynRegistry.
impl<T: Registry> DynRegistry for T {
    fn lookup<'a>(
        &'a self,
        crate_name: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<IndexEntry>, RegistryError>> + Send + 'a>> {
        Box::pin(Registry::lookup(self, crate_name))
    }

    fn download<'a>(
        &'a self,
        crate_name: &'a str,
        version: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, RegistryError>> + Send + 'a>> {
        Box::pin(Registry::download(self, crate_name, version))
    }

    fn publish<'a>(
        &'a self,
        metadata: PublishMetadata,
        crate_data: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = Result<String, RegistryError>> + Send + 'a>> {
        Box::pin(Registry::publish(self, metadata, crate_data))
    }
}

/// A type-erased registry wrapper that implements `Registry`.
///
/// This allows storing any registry as a concrete type while still being
/// able to use it through the `Registry` trait. Uses `Arc` internally
/// so it can be cloned cheaply.
#[derive(Clone)]
pub struct AnyRegistry(Arc<dyn DynRegistry>);

impl AnyRegistry {
    /// Create a new `AnyRegistry` from any type implementing `Registry`.
    pub fn new<R: Registry + 'static>(registry: R) -> Self {
        Self(Arc::new(registry))
    }

    /// Create a new `AnyRegistry` from an `Arc<dyn DynRegistry>`.
    pub fn from_arc(registry: Arc<dyn DynRegistry>) -> Self {
        Self(registry)
    }
}

impl Registry for AnyRegistry {
    async fn lookup(&self, crate_name: &str) -> Result<Vec<IndexEntry>, RegistryError> {
        self.0.lookup(crate_name).await
    }

    async fn download(&self, crate_name: &str, version: &str) -> Result<Vec<u8>, RegistryError> {
        self.0.download(crate_name, version).await
    }

    async fn publish(
        &self,
        metadata: PublishMetadata,
        crate_data: &[u8],
    ) -> Result<String, RegistryError> {
        self.0.publish(metadata, crate_data).await
    }
}
