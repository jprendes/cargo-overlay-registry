use super::{Registry, RegistryError};
use crate::types::{IndexEntry, PublishMetadata};

/// An overlay registry that combines two registries.
///
/// - Writes (publish) go to the top layer
/// - Reads check the top layer first, then fall back to the bottom layer
/// - Index lookups merge entries from both layers (top takes precedence for same version)
pub struct OverlayRegistry<Top, Bottom>
where
    Top: Registry,
    Bottom: Registry,
{
    /// The writable top layer (e.g., LocalRegistry)
    pub top: Top,
    /// The read-only bottom layer (e.g., RemoteRegistry)
    pub bottom: Bottom,
}

impl<Top, Bottom> OverlayRegistry<Top, Bottom>
where
    Top: Registry,
    Bottom: Registry,
{
    pub fn new(top: Top, bottom: Bottom) -> Self {
        Self { top, bottom }
    }
}

impl<Top, Bottom> Registry for OverlayRegistry<Top, Bottom>
where
    Top: Registry,
    Bottom: Registry,
{
    async fn lookup(&self, crate_name: &str) -> Result<Vec<IndexEntry>, RegistryError> {
        // Get entries from both layers
        let top_entries = self.top.lookup(crate_name).await?;
        let bottom_entries = self.bottom.lookup(crate_name).await?;

        // Merge: start with bottom entries, then add/replace with top entries
        let mut merged: Vec<IndexEntry> = bottom_entries;

        for top_entry in top_entries {
            // Remove any existing entry with the same version
            merged.retain(|e| e.vers != top_entry.vers);
            // Add the top entry
            merged.push(top_entry);
        }

        Ok(merged)
    }

    async fn download(&self, crate_name: &str, version: &str) -> Result<Vec<u8>, RegistryError> {
        // Try top layer first
        match self.top.download(crate_name, version).await {
            Ok(data) => return Ok(data),
            Err(RegistryError::NotFound) => {
                // Fall through to bottom layer
            }
            Err(e) => return Err(e),
        }

        // Fall back to bottom layer
        self.bottom.download(crate_name, version).await
    }

    async fn publish(
        &self,
        metadata: PublishMetadata,
        crate_data: &[u8],
    ) -> Result<String, RegistryError> {
        // Publish always goes to the top layer
        self.top.publish(metadata, crate_data).await
    }
}
