use std::path::PathBuf;

use super::{AnyRegistry, LocalRegistry, OverlayRegistry, RemoteRegistry};

/// Registry specification for building overlay registries
#[derive(Debug, Clone)]
pub enum RegistrySpec {
    /// Local filesystem registry
    /// Path is optional, defaults to a temporary directory
    /// read_only controls whether the registry accepts publishes
    Local { path: Option<PathBuf>, read_only: bool },
    /// Remote registry (read-only)
    Remote { api_url: String, index_url: String },
}

impl RegistrySpec {
    /// Shortcut for crates.io remote registry
    pub fn crates_io() -> Self {
        RegistrySpec::Remote {
            api_url: "https://crates.io".to_string(),
            index_url: "https://index.crates.io".to_string(),
        }
    }

    /// Create a local registry spec with a specific path
    pub fn local(path: impl Into<PathBuf>) -> Self {
        RegistrySpec::Local {
            path: Some(path.into()),
            read_only: false,
        }
    }

    /// Create a local registry spec that will use a temporary directory
    pub fn local_temp() -> Self {
        RegistrySpec::Local {
            path: None,
            read_only: false,
        }
    }

    /// Create a read-only local registry spec with a specific path
    pub fn local_read_only(path: impl Into<PathBuf>) -> Self {
        RegistrySpec::Local {
            path: Some(path.into()),
            read_only: true,
        }
    }

    /// Create a remote registry spec
    pub fn remote(api_url: impl Into<String>, index_url: impl Into<String>) -> Self {
        RegistrySpec::Remote {
            api_url: api_url.into(),
            index_url: index_url.into(),
        }
    }
}

impl std::str::FromStr for RegistrySpec {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Handle shortcuts
        if s == "crates.io" || s == "crates-io" {
            return Ok(RegistrySpec::crates_io());
        }

        // Parse type=value format
        let (reg_type, value) = if let Some(idx) = s.find('=') {
            (&s[..idx], Some(&s[idx + 1..]))
        } else {
            (s, None)
        };

        match reg_type {
            "local" => {
                let (path, read_only) = match value {
                    Some(v) if v.ends_with(",ro") => {
                        let path_str = &v[..v.len() - 3];
                        (
                            if path_str.is_empty() {
                                None
                            } else {
                                Some(PathBuf::from(path_str))
                            },
                            true,
                        )
                    }
                    Some(v) => (Some(PathBuf::from(v)), false),
                    None => (None, false),
                };
                Ok(RegistrySpec::Local { path, read_only })
            }
            "remote" => {
                let value = value.ok_or("remote registry requires a URL")?;
                let (api_url, index_url) = if let Some(idx) = value.find(',') {
                    (value[..idx].to_string(), value[idx + 1..].to_string())
                } else {
                    // If only one URL, use it for both API and index
                    (value.to_string(), value.to_string())
                };
                Ok(RegistrySpec::Remote { api_url, index_url })
            }
            _ => Err(format!(
                "unknown registry type '{}'. Use: local, remote, or crates.io",
                reg_type
            )),
        }
    }
}

/// Options for building a registry from specs
#[derive(Debug, Clone, Default)]
pub struct RegistryBuildOptions {
    /// Skip metadata validation on the topmost local registry
    pub permissive_publishing: bool,
}

/// Result of building a registry from specs
pub struct BuiltRegistry {
    /// The constructed overlay registry
    pub registry: AnyRegistry,
    /// Upstream hosts extracted from remote registries (for MITM interception)
    pub upstream_hosts: Vec<String>,
    /// Temporary directories created for local registries (must be kept alive)
    pub temp_dirs: Vec<tempfile::TempDir>,
}

impl BuiltRegistry {
    /// Get the upstream API URL (from the bottom-most remote registry)
    pub fn upstream_api(&self, specs: &[RegistrySpec]) -> String {
        for spec in specs.iter().rev() {
            if let RegistrySpec::Remote { api_url, .. } = spec {
                return api_url.clone();
            }
        }
        "https://crates.io".to_string()
    }
}

/// Build an overlay registry from a list of specs (first = top, last = bottom)
pub fn build_registry(specs: &[RegistrySpec], options: &RegistryBuildOptions) -> BuiltRegistry {
    let mut upstream_hosts = Vec::new();
    let mut temp_dirs = Vec::new();

    // Find the index of the topmost (first) local registry
    let topmost_local_idx = specs
        .iter()
        .position(|s| matches!(s, RegistrySpec::Local { .. }));

    // Build from bottom to top
    let mut registry: Option<AnyRegistry> = None;

    for (idx, spec) in specs.iter().enumerate().rev() {
        let layer: AnyRegistry = match spec {
            RegistrySpec::Local { path, read_only } => {
                let path = path.clone().unwrap_or_else(|| {
                    let temp_dir =
                        tempfile::tempdir().expect("Failed to create temporary directory");
                    let path = temp_dir.path().to_path_buf();
                    temp_dirs.push(temp_dir);
                    path
                });

                // Create index directory
                std::fs::create_dir_all(path.join("index")).ok();

                if *read_only {
                    AnyRegistry::new(LocalRegistry::read_only(path))
                } else {
                    // The topmost local registry gets permissive publishing if the flag is set
                    let is_topmost = topmost_local_idx == Some(idx);
                    let validate = !(options.permissive_publishing && is_topmost);
                    AnyRegistry::new(LocalRegistry::new(path, validate))
                }
            }
            RegistrySpec::Remote { api_url, index_url } => {
                // Extract hosts for MITM interception
                if let Ok(url) = url::Url::parse(api_url) {
                    if let Some(host) = url.host_str() {
                        if !upstream_hosts.contains(&host.to_string()) {
                            upstream_hosts.push(host.to_string());
                        }
                    }
                }
                if let Ok(url) = url::Url::parse(index_url) {
                    if let Some(host) = url.host_str() {
                        if !upstream_hosts.contains(&host.to_string()) {
                            upstream_hosts.push(host.to_string());
                        }
                    }
                }
                AnyRegistry::new(RemoteRegistry::new(index_url.clone(), api_url.clone()))
            }
        };

        registry = Some(match registry {
            None => layer,
            Some(bottom) => AnyRegistry::new(OverlayRegistry::new(layer, bottom)),
        });
    }

    // Add static.crates.io if crates.io is in hosts
    if upstream_hosts.iter().any(|h| h.contains("crates.io")) {
        if !upstream_hosts.contains(&"static.crates.io".to_string()) {
            upstream_hosts.push("static.crates.io".to_string());
        }
    }

    BuiltRegistry {
        registry: registry.expect("At least one registry must be specified"),
        upstream_hosts,
        temp_dirs,
    }
}
