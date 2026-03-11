use super::{Registry, RegistryError};
use crate::types::IndexEntry;
use reqwest::Client;

/// A remote registry that fetches from an upstream like crates.io
pub struct RemoteRegistry {
    /// HTTP client for making requests
    client: Client,
    /// Upstream sparse index URL (e.g., https://index.crates.io)
    pub index_url: String,
    /// Upstream API URL (e.g., https://crates.io)
    pub api_url: String,
}

impl RemoteRegistry {
    pub fn new(index_url: String, api_url: String) -> Self {
        Self {
            client: Client::builder()
                .user_agent("cargo-overlay-registry/0.1.0")
                .build()
                .expect("Failed to create HTTP client"),
            index_url,
            api_url,
        }
    }

    /// Get the index path for a crate name (sparse index format)
    fn index_path(crate_name: &str) -> String {
        let name_lower = crate_name.to_lowercase();
        match name_lower.len() {
            1 => format!("/1/{}", name_lower),
            2 => format!("/2/{}", name_lower),
            3 => format!("/3/{}/{}", &name_lower[..1], name_lower),
            _ => format!("/{}/{}/{}", &name_lower[..2], &name_lower[2..4], name_lower),
        }
    }
}

impl Registry for RemoteRegistry {
    async fn lookup(&self, crate_name: &str) -> Result<Vec<IndexEntry>, RegistryError> {
        let path = Self::index_path(crate_name);
        let url = format!("{}{}", self.index_url, path);

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| RegistryError::Network(e.to_string()))?;

        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(Vec::new());
        }

        if !response.status().is_success() {
            return Err(RegistryError::Network(format!(
                "upstream returned {}",
                response.status()
            )));
        }

        let body = response
            .text()
            .await
            .map_err(|e| RegistryError::Network(e.to_string()))?;

        let entries: Vec<IndexEntry> = body
            .lines()
            .filter(|line| !line.is_empty())
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();

        Ok(entries)
    }

    async fn download(&self, crate_name: &str, version: &str) -> Result<Vec<u8>, RegistryError> {
        let url = format!(
            "{}/api/v1/crates/{}/{}/download",
            self.api_url, crate_name, version
        );

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| RegistryError::Network(e.to_string()))?;

        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(RegistryError::NotFound);
        }

        if !response.status().is_success() {
            return Err(RegistryError::Network(format!(
                "upstream returned {}",
                response.status()
            )));
        }

        let bytes = response
            .bytes()
            .await
            .map_err(|e| RegistryError::Network(e.to_string()))?;

        Ok(bytes.to_vec())
    }

    async fn publish(
        &self,
        _metadata: crate::types::PublishMetadata,
        _crate_data: &[u8],
    ) -> Result<String, RegistryError> {
        // Remote registries don't support publishing through this interface
        Err(RegistryError::NotSupported)
    }
}
