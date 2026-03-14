use std::collections::HashMap;

use serde::{Deserialize, Serialize};

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

impl PublishMetadata {
    /// Validate metadata according to crates.io requirements.
    /// Returns a list of validation errors, empty if valid.
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();

        // Description is required
        match &self.description {
            None => errors.push("missing field `description`".to_string()),
            Some(d) if d.trim().is_empty() => {
                errors.push("the `description` field must not be empty".to_string())
            }
            _ => {}
        }

        // License or license_file required
        let has_license = self.license.as_ref().is_some_and(|l| !l.trim().is_empty());
        let has_license_file = self
            .license_file
            .as_ref()
            .is_some_and(|l| !l.trim().is_empty());
        if !has_license && !has_license_file {
            errors.push(
                "missing field `license` or `license-file` (at least one is required)".to_string(),
            );
        }

        // At least one of documentation, homepage, or repository is required
        let has_documentation = self
            .documentation
            .as_ref()
            .is_some_and(|d| !d.trim().is_empty());
        let has_homepage = self.homepage.as_ref().is_some_and(|h| !h.trim().is_empty());
        let has_repository = self
            .repository
            .as_ref()
            .is_some_and(|r| !r.trim().is_empty());
        if !has_documentation && !has_homepage && !has_repository {
            errors.push(
                "missing field `documentation`, `homepage`, or `repository` (at least one is required)".to_string(),
            );
        }

        // Keywords: max 5, each max 20 chars, ASCII alphanumeric + - + _
        if self.keywords.len() > 5 {
            errors.push(format!(
                "too many keywords: {} (max 5)",
                self.keywords.len()
            ));
        }
        for kw in &self.keywords {
            if kw.len() > 20 {
                errors.push(format!(
                    "keyword `{}` is too long: {} chars (max 20)",
                    kw,
                    kw.len()
                ));
            }
            if !kw
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                errors.push(format!(
                    "keyword `{}` contains invalid characters (only ASCII alphanumeric, `-`, `_` allowed)",
                    kw
                ));
            }
            if kw.starts_with('-') || kw.starts_with('_') {
                errors.push(format!(
                    "keyword `{}` must start with a letter or number",
                    kw
                ));
            }
        }

        // Categories: max 5
        if self.categories.len() > 5 {
            errors.push(format!(
                "too many categories: {} (max 5)",
                self.categories.len()
            ));
        }

        // Crate name validation
        if self.name.is_empty() {
            errors.push("crate name cannot be empty".to_string());
        } else if self.name.len() > 64 {
            errors.push(format!(
                "crate name is too long: {} chars (max 64)",
                self.name.len()
            ));
        } else {
            let first = self.name.chars().next().unwrap();
            if !first.is_ascii_alphabetic() && first != '_' {
                errors.push("crate name must start with a letter or underscore".to_string());
            }
            if !self
                .name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                errors.push(
                    "crate name contains invalid characters (only ASCII alphanumeric, `-`, `_` allowed)"
                        .to_string(),
                );
            }
        }

        errors
    }
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
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_features2"
    )]
    pub features2: Option<HashMap<String, Vec<String>>>,
    #[serde(default, deserialize_with = "deserialize_yanked")]
    pub yanked: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub links: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rust_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub v: Option<u32>,
}

/// Deserialize features2 which can be null, missing, or a map
fn deserialize_features2<'de, D>(
    deserializer: D,
) -> Result<Option<HashMap<String, Vec<String>>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<HashMap<String, Vec<String>>>::deserialize(deserializer)
}

/// Deserialize yanked which can be null, missing, or a boolean (null/missing -> false)
fn deserialize_yanked<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<bool>::deserialize(deserializer).map(|opt| opt.unwrap_or(false))
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
    // Additional fields cargo may include (we ignore them for serialization)
    #[serde(default, skip_serializing)]
    pub public: Option<bool>,
    #[serde(default, skip_serializing)]
    pub artifact: Option<serde_json::Value>,
    #[serde(default, skip_serializing)]
    pub bindep_target: Option<String>,
    #[serde(default, skip_serializing)]
    pub lib: Option<bool>,
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
