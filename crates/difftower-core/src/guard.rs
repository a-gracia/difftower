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
///
/// Rules are globs relative to the repository root. As in `.gitignore`, a
/// rule starting with `!` is an exception, and the last rule that matches a
/// path decides whether it is protected.
#[derive(Clone)]
pub struct Guard {
    patterns: Vec<String>,
    /// Whether each pattern is an exception (`!`).
    negated: Vec<bool>,
    set: GlobSet,
}

impl Guard {
    pub fn new(patterns: Vec<String>) -> Result<Self> {
        let mut builder = GlobSetBuilder::new();
        let mut negated = Vec::with_capacity(patterns.len());
        for pattern in &patterns {
            let glob = pattern.strip_prefix('!');
            negated.push(glob.is_some());
            builder.add(Glob::new(glob.unwrap_or(pattern))?);
        }
        Ok(Self { patterns, negated, set: builder.build()? })
    }

    /// A guard that protects nothing.
    pub fn empty() -> Self {
        Self::new(Vec::new()).expect("empty glob set")
    }

    pub fn patterns(&self) -> &[String] {
        &self.patterns
    }

    pub fn is_protected(&self, path: &str) -> bool {
        self.rule_for(path).is_some()
    }

    /// The index into [`Self::patterns`] of the rule that protects `path`,
    /// or `None` when nothing does or an exception wins.
    pub fn rule_for(&self, path: &str) -> Option<usize> {
        let last = self.set.matches(path).into_iter().max()?;
        (!self.negated[last]).then_some(last)
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

    /// This guard with a file, or a directory and everything in it, protected
    /// or not. Rules for paths inside the target are dropped first, so the
    /// result is the shortest rule list that does it: the target's own rule,
    /// or an exception when an outer rule would still protect it.
    pub fn with_protected(&self, target: &str, is_dir: bool, protect: bool) -> Result<Self> {
        let escaped = globset::escape(target);
        let inside = format!("{escaped}/");
        let mut patterns: Vec<String> = self
            .patterns
            .iter()
            .filter(|pattern| {
                let glob = pattern.strip_prefix('!').unwrap_or(pattern);
                glob != escaped && !(is_dir && glob.starts_with(&inside))
            })
            .cloned()
            .collect();
        let probe = if is_dir { inside } else { target.to_owned() };
        if Self::new(patterns.clone())?.is_protected(&probe) != protect {
            let rule = if is_dir { dir_rule(target) } else { file_rule(target) };
            patterns.push(if protect { rule } else { format!("!{rule}") });
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
