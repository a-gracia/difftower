use std::path::Path;

use anyhow::Result;
use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};

/// Per-repository settings, committed so a team shares its guards.
pub const CONFIG_FILE: &str = ".difftower.toml";

#[derive(Default, Serialize, Deserialize)]
struct Config {
    #[serde(default)]
    guard: GuardConfig,
}

#[derive(Default, Serialize, Deserialize)]
struct GuardConfig {
    /// Glob patterns, relative to the repository root, that agents must not change.
    #[serde(default)]
    protect: Vec<String>,
}

/// The set of protected paths of one repository.
#[derive(Clone)]
pub struct Guard {
    patterns: Vec<String>,
    set: GlobSet,
}

impl Guard {
    pub fn new(patterns: Vec<String>) -> Result<Self> {
        let mut builder = GlobSetBuilder::new();
        for pattern in &patterns {
            builder.add(Glob::new(pattern)?);
        }
        Ok(Self { patterns, set: builder.build()? })
    }

    /// A guard that protects nothing.
    pub fn empty() -> Self {
        Self::new(Vec::new()).expect("empty glob set")
    }

    pub fn patterns(&self) -> &[String] {
        &self.patterns
    }

    pub fn is_protected(&self, path: &str) -> bool {
        self.set.is_match(path)
    }

    /// Indices into [`Self::patterns`] of the rules that protect `path`.
    pub fn rules_for(&self, path: &str) -> Vec<usize> {
        self.set.matches(path)
    }

    pub fn has_rule(&self, pattern: &str) -> bool {
        self.patterns.iter().any(|p| p == pattern)
    }

    /// This guard with one rule added or removed.
    pub fn with_rule(&self, pattern: &str, enabled: bool) -> Result<Self> {
        let mut patterns = self.patterns.clone();
        patterns.retain(|p| p != pattern);
        if enabled {
            patterns.push(pattern.to_owned());
        }
        Self::new(patterns)
    }
}

/// The rule that protects exactly one file.
pub fn file_rule(path: &str) -> String {
    globset::escape(path)
}

/// The rule that protects everything inside a directory.
pub fn dir_rule(dir: &str) -> String {
    format!("{}/**", globset::escape(dir))
}

/// Reads the protection rules from the repository's configuration file.
pub fn load_rules(root: &Path) -> Result<Vec<String>> {
    let config: Config = match std::fs::read_to_string(root.join(CONFIG_FILE)) {
        Ok(text) => toml::from_str(&text)?,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Config::default(),
        Err(err) => return Err(err.into()),
    };
    Ok(config.guard.protect)
}

/// Writes the protection rules, keeping any other settings in the file.
pub fn save_rules(root: &Path, patterns: &[String]) -> Result<()> {
    let path = root.join(CONFIG_FILE);
    let mut doc: toml::Table = match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text)?,
        Err(_) => toml::Table::new(),
    };
    let guard = doc.entry("guard").or_insert_with(|| toml::Value::Table(toml::Table::new()));
    if let toml::Value::Table(guard) = guard {
        guard.insert(
            "protect".into(),
            toml::Value::Array(patterns.iter().cloned().map(toml::Value::String).collect()),
        );
    }
    std::fs::write(&path, toml::to_string_pretty(&doc)?)?;
    Ok(())
}
