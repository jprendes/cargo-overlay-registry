use std::path::PathBuf;

use super::{Registry, RegistryError};
use crate::types::IndexEntry;

/// A read-only registry for cargo's tmp-registry created during `cargo publish`.
/// 
/// Cargo creates this registry at `{target_dir}/package/tmp-registry` to make
/// packaged crates available during the verification step. This allows build
/// scripts that call `cargo metadata` to resolve workspace dependencies.
/// 
/// The tmp-registry has a specific format:
/// - Index files: `index/{prefix}/{name}` (same as standard registries)
/// - Crate files: `{name}-{version}.crate` (directly in root, not in subdirs)
#[derive(Clone)]
pub struct PublishRegistry {
    /// Path to the tmp-registry directory
    pub path: PathBuf,
}

impl PublishRegistry {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Get the path to an index file for a crate name
    fn index_path(&self, crate_name: &str) -> PathBuf {
        let name_lower = crate_name.to_lowercase();
        match name_lower.len() {
            1 => self.path.join("index").join("1").join(&name_lower),
            2 => self.path.join("index").join("2").join(&name_lower),
            3 => self
                .path
                .join("index")
                .join("3")
                .join(&name_lower[..1])
                .join(&name_lower),
            _ => self
                .path
                .join("index")
                .join(&name_lower[..2])
                .join(&name_lower[2..4])
                .join(&name_lower),
        }
    }

    /// Get the path to a crate file.
    /// tmp-registry stores crates as `{name}-{version}.crate` directly in the root.
    fn crate_path(&self, crate_name: &str, version: &str) -> PathBuf {
        self.path.join(format!("{}-{}.crate", crate_name, version))
    }
}

impl Registry for PublishRegistry {
    async fn lookup(&self, crate_name: &str) -> Result<Vec<IndexEntry>, RegistryError> {
        let index_path = self.index_path(crate_name);

        if !index_path.exists() {
            return Ok(Vec::new());
        }

        let content = match tokio::fs::read_to_string(&index_path).await {
            Ok(c) => c,
            Err(_) => return Ok(Vec::new()),
        };

        let entries: Vec<IndexEntry> = content
            .lines()
            .filter(|line| !line.is_empty())
            .filter_map(|line| {
                match serde_json::from_str(line) {
                    Ok(entry) => Some(entry),
                    Err(e) => {
                        log::debug!("[PublishRegistry] Failed to parse index entry: {}", e);
                        None
                    }
                }
            })
            .collect();

        Ok(entries)
    }

    async fn download(&self, crate_name: &str, version: &str) -> Result<Vec<u8>, RegistryError> {
        let crate_path = self.crate_path(crate_name, version);

        if !crate_path.exists() {
            return Err(RegistryError::NotFound);
        }

        let data = tokio::fs::read(&crate_path).await?;
        Ok(data)
    }

    async fn publish(
        &self,
        _metadata: crate::types::PublishMetadata,
        _crate_data: &[u8],
    ) -> Result<String, RegistryError> {
        // PublishRegistry is read-only
        Err(RegistryError::NotFound)
    }
}
