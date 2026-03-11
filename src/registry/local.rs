use std::path::PathBuf;

use sha2::{Digest, Sha256};

use super::{Registry, RegistryError};
use crate::types::{IndexDependency, IndexEntry, PublishMetadata};

/// A local filesystem-based registry
pub struct LocalRegistry {
    /// Path to the registry storage directory
    pub path: PathBuf,
    /// Whether to validate metadata on publish
    pub validate_metadata: bool,
}

impl LocalRegistry {
    pub fn new(path: PathBuf, validate_metadata: bool) -> Self {
        Self {
            path,
            validate_metadata,
        }
    }

    /// Get the path to a crate file
    fn crate_path(&self, crate_name: &str, version: &str) -> PathBuf {
        self.path
            .join("crates")
            .join(crate_name)
            .join(format!("{}.crate", version))
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
}

impl Registry for LocalRegistry {
    async fn lookup(&self, crate_name: &str) -> Result<Vec<IndexEntry>, RegistryError> {
        let index_path = self.index_path(crate_name);

        if !index_path.exists() {
            return Ok(Vec::new());
        }

        let content = tokio::fs::read_to_string(&index_path).await?;
        let entries: Vec<IndexEntry> = content
            .lines()
            .filter(|line| !line.is_empty())
            .filter_map(|line| serde_json::from_str(line).ok())
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
        metadata: PublishMetadata,
        crate_data: &[u8],
    ) -> Result<String, RegistryError> {
        // Validate metadata if enabled
        if self.validate_metadata {
            let errors = metadata.validate();
            if !errors.is_empty() {
                return Err(RegistryError::ValidationFailed(errors));
            }
        }

        // Compute SHA256 checksum
        let mut hasher = Sha256::new();
        hasher.update(crate_data);
        let checksum = format!("{:x}", hasher.finalize());

        // Save the .crate file
        let crate_path = self.crate_path(&metadata.name, &metadata.vers);
        if let Some(parent) = crate_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&crate_path, crate_data).await?;

        // Create index entry
        let index_entry = IndexEntry {
            name: metadata.name.clone(),
            vers: metadata.vers.clone(),
            deps: metadata
                .deps
                .into_iter()
                .map(|d| {
                    let (name, package) = if let Some(alias) = d.explicit_name_in_toml {
                        (alias, Some(d.name))
                    } else {
                        (d.name, None)
                    };
                    IndexDependency {
                        name,
                        req: d.version_req,
                        features: d.features,
                        optional: d.optional,
                        default_features: d.default_features,
                        target: d.target,
                        kind: d.kind,
                        registry: d.registry,
                        package,
                    }
                })
                .collect(),
            cksum: checksum.clone(),
            features: metadata.features,
            yanked: false,
            links: metadata.links,
            rust_version: metadata.rust_version,
        };

        // Write to index
        let index_path = self.index_path(&metadata.name);
        if let Some(parent) = index_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        // Read existing, filter out same version, append new
        let mut lines: Vec<String> = if index_path.exists() {
            let content = tokio::fs::read_to_string(&index_path).await?;
            content
                .lines()
                .filter(|line| {
                    if let Ok(entry) = serde_json::from_str::<IndexEntry>(line) {
                        entry.vers != metadata.vers
                    } else {
                        true
                    }
                })
                .map(|s| s.to_string())
                .collect()
        } else {
            Vec::new()
        };
        lines.push(serde_json::to_string(&index_entry)?);

        let index_content = lines.join("\n") + "\n";
        tokio::fs::write(&index_path, index_content).await?;

        Ok(checksum)
    }
}
