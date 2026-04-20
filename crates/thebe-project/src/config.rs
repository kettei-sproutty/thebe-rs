use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use anyhow::Context;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub struct TailwindConfig {
    pub input: String,
    pub output: String,
}

/// Parsed Thebe configuration from `thebe.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub struct ThebeConfig {
    /// Hook commands indexed by lifecycle event (e.g., "pre_build", "on_change").
    #[serde(default)]
    pub hooks: HashMap<String, String>,
    /// Configuration for Tailwind CSS integration.
    #[serde(default)]
    pub tailwind: Option<TailwindConfig>,
    /// Additional configuration tables for future extensibility.
    #[serde(flatten)]
    pub extra: HashMap<String, toml::Value>,
}

impl ThebeConfig {
    /// Load configuration from `thebe.toml` in the project root.
    ///
    /// Returns an empty config if `thebe.toml` does not exist.
    /// Propagates parse errors if the file is malformed.
    pub fn load(project_root: &Path) -> anyhow::Result<Self> {
        let config_path = project_root.join("thebe.toml");

        if !config_path.exists() {
            return Ok(Self::default());
        }

        let source = std::fs::read_to_string(&config_path)
            .with_context(|| format!("failed to read {}", config_path.display()))?;

        toml::from_str(&source)
            .with_context(|| format!("failed to parse {}", config_path.display()))
    }

    /// Get a hook command by name, if it exists.
    #[must_use]
    pub fn get_hook(&self, name: &str) -> Option<&str> {
        self.hooks.get(name).map(|s| s.as_str())
    }
}
