use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Publish request metadata (from cargo)
#[derive(Deserialize, Debug)]
#[allow(dead_code)]
pub struct PublishMetadata {
    pub name: String,
    pub vers: String,
    #[serde(default)]
    pub deps: Vec<PublishDependency>,
    #[serde(default)]
    pub features: HashMap<String, Vec<String>>,
    #[serde(default)]
    pub authors: Vec<String>,
    pub description: Option<String>,
    pub documentation: Option<String>,
    pub homepage: Option<String>,
    pub readme: Option<String>,
    pub readme_file: Option<String>,
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default)]
    pub categories: Vec<String>,
    pub license: Option<String>,
    pub license_file: Option<String>,
    pub repository: Option<String>,
    pub links: Option<String>,
    pub rust_version: Option<String>,
}

/// Dependency in publish request
#[derive(Deserialize, Debug)]
pub struct PublishDependency {
    pub name: String,
    pub version_req: String,
    #[serde(default)]
    pub features: Vec<String>,
    #[serde(default)]
    pub optional: bool,
    #[serde(default = "default_true")]
    pub default_features: bool,
    pub target: Option<String>,
    pub kind: Option<String>,
    pub registry: Option<String>,
    pub explicit_name_in_toml: Option<String>,
}

fn default_true() -> bool {
    true
}

/// Index entry for a crate version
#[derive(Serialize, Deserialize, Debug)]
pub struct IndexEntry {
    pub name: String,
    pub vers: String,
    pub deps: Vec<IndexDependency>,
    pub cksum: String,
    pub features: HashMap<String, Vec<String>>,
    #[serde(default)]
    pub yanked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub links: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rust_version: Option<String>,
}

/// Dependency in index entry
#[derive(Serialize, Deserialize, Debug)]
pub struct IndexDependency {
    pub name: String,
    pub req: String,
    pub features: Vec<String>,
    pub optional: bool,
    pub default_features: bool,
    pub target: Option<String>,
    pub kind: Option<String>,
    pub registry: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
}

/// Publish response
#[derive(Serialize)]
pub struct PublishResponse {
    pub warnings: PublishWarnings,
}

#[derive(Serialize)]
pub struct PublishWarnings {
    pub invalid_categories: Vec<String>,
    pub invalid_badges: Vec<String>,
    pub other: Vec<String>,
}

/// Custom config.json that points cargo to our proxy
#[derive(Serialize, Deserialize)]
pub struct RegistryConfig {
    pub dl: String,
    pub api: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "auth-required")]
    pub auth_required: Option<bool>,
}
