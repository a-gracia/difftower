use std::path::{Path, PathBuf};

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
pub struct Guard {
    path: PathBuf,
    patterns: Vec<String>,
    set: GlobSet,
}

impl Guard {
    pub fn load(root: &Path) -> Result<Self> {
        let path = root.join(CONFIG_FILE);
        let config: Config = match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text)?,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Config::default(),
            Err(err) => return Err(err.into()),
        };
        Self::new(path, config.guard.protect)
    }

    /// A guard that protects nothing, used when the configuration is unreadable.
    pub fn empty(root: &Path) -> Self {
        Self::new(root.join(CONFIG_FILE), Vec::new()).expect("empty glob set")
    }

    fn new(path: PathBuf, patterns: Vec<String>) -> Result<Self> {
        let mut builder = GlobSetBuilder::new();
        for pattern in &patterns {
            builder.add(Glob::new(pattern)?);
        }
        Ok(Self { path, patterns, set: builder.build()? })
    }

    pub fn patterns(&self) -> &[String] {
        &self.patterns
    }

    pub fn is_protected(&self, path: &str) -> bool {
        self.set.is_match(path)
    }

    /// Whether `path` is protected by itself rather than by a wider glob.
    pub fn is_pinned(&self, path: &str) -> bool {
        self.patterns.iter().any(|p| p == path)
    }

    /// Pins or unpins one exact path and saves the configuration.
    pub fn set_pinned(&mut self, path: &str, pinned: bool) -> Result<()> {
        let mut patterns = self.patterns.clone();
        patterns.retain(|p| p != path);
        if pinned {
            patterns.push(globset::escape(path));
        }
        *self = Self::new(self.path.clone(), patterns)?;
        self.save()
    }

    fn save(&self) -> Result<()> {
        // Keep any other sections a user wrote by hand.
        let mut doc: toml::Table = match std::fs::read_to_string(&self.path) {
            Ok(text) => toml::from_str(&text)?,
            Err(_) => toml::Table::new(),
        };
        let guard = doc
            .entry("guard")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if let toml::Value::Table(guard) = guard {
            guard.insert(
                "protect".into(),
                toml::Value::Array(self.patterns.iter().cloned().map(toml::Value::String).collect()),
            );
        }
        std::fs::write(&self.path, toml::to_string_pretty(&doc)?)?;
        Ok(())
    }
}
