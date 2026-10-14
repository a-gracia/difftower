//! Several repositories side by side in a directory that is not a
//! repository itself, shown as one. Every path gets its repository's
//! directory name as a prefix; each repository keeps its own rules and
//! approvals, so nothing is written to the parent directory.

use std::{
    any::Any,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, anyhow, bail};

use crate::{
    Backend, Change, Repo, Status,
    backend::{dir_name, repo_status},
    guard,
};

pub struct Multi {
    root: PathBuf,
    /// Each repository with its directory name, sorted by name.
    repos: Vec<(String, Repo)>,
}

impl Multi {
    /// The repositories directly inside `root`, or `None` if there are none.
    pub fn discover(root: &Path) -> Result<Option<Self>> {
        let mut repos = Vec::new();
        for entry in std::fs::read_dir(root)? {
            let entry = entry?;
            let path = entry.path();
            if !entry.file_type()?.is_dir() || !path.join(".git").exists() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            repos.push((name, Repo::discover(&path)?));
        }
        if repos.is_empty() {
            return Ok(None);
        }
        repos.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(Some(Self { root: root.to_path_buf(), repos }))
    }

    /// The repository holding `path`, and `path` relative to it.
    fn locate<'a>(&self, path: &'a str) -> Result<(&Repo, &'a str)> {
        self.repos
            .iter()
            .find_map(|(name, repo)| Some((repo, path.strip_prefix(name.as_str())?.strip_prefix('/')?)))
            .ok_or_else(|| anyhow!("{path} is not inside any of the repositories"))
    }
}

impl Backend for Multi {
    fn name(&self) -> String {
        dir_name(&self.root())
    }

    fn host(&self) -> Option<String> {
        None
    }

    fn root(&self) -> String {
        self.root.display().to_string()
    }

    fn repo_count(&self) -> usize {
        self.repos.len()
    }

    fn status(&self) -> Result<Status> {
        let statuses: Vec<Result<Status>> = std::thread::scope(|scope| {
            let handles: Vec<_> = self
                .repos
                .iter()
                .map(|(name, repo)| scope.spawn(move || repo_status(repo, name).with_context(|| name.clone())))
                .collect();
            handles.into_iter().map(|h| h.join().expect("status thread panicked")).collect()
        });
        let mut status = Status {
            changes: Vec::new(),
            files: Vec::new(),
            rules: Vec::new(),
            rules_error: None,
            approvals: String::new(),
        };
        let mut errors = Vec::new();
        for ((name, _), repo_status) in self.repos.iter().zip(statuses) {
            let repo_status = repo_status?;
            status.changes.extend(repo_status.changes);
            status.files.extend(repo_status.files.iter().map(|f| format!("{name}/{f}")));
            let dir = globset::escape(name);
            status.rules.extend(repo_status.rules.iter().map(|r| format!("{dir}/{r}")));
            if let Some(err) = repo_status.rules_error {
                errors.push(format!("{name}: {err}"));
            }
            for line in repo_status.approvals.lines() {
                if let Some((hash, path)) = line.split_once('\t') {
                    status.approvals.push_str(&format!("{hash}\t{name}/{path}\n"));
                }
            }
        }
        if !errors.is_empty() {
            status.rules_error = Some(errors.join("; "));
        }
        Ok(status)
    }

    fn revert(&self, change: &Change) -> Result<()> {
        let (repo, path) = self.locate(&change.path)?;
        let old_path = match &change.old_path {
            Some(old) => Some(self.locate(old)?.1.to_owned()),
            None => None,
        };
        let change = Change { path: path.to_owned(), old_path, ..change.clone() };
        repo.revert(&repo.head_tree()?, &change)
    }

    fn read_file(&self, path: &str) -> Result<Vec<u8>> {
        let (repo, path) = self.locate(path)?;
        Ok(std::fs::read(repo.root().join(path))?)
    }

    /// Splits the rules by repository, writing only the files that change.
    fn save_rules(&self, rules: &[String]) -> Result<()> {
        let mut by_repo: Vec<Vec<String>> = vec![Vec::new(); self.repos.len()];
        for rule in rules {
            let ix = self
                .repos
                .iter()
                .position(|(name, _)| rule.starts_with(&format!("{}/", globset::escape(name))))
                .with_context(|| format!("the rule {rule} must be inside one of the repositories"))?;
            let dir_len = globset::escape(&self.repos[ix].0).len() + 1;
            by_repo[ix].push(rule[dir_len..].to_owned());
        }
        for ((_, repo), rules) in self.repos.iter().zip(by_repo) {
            if guard::load_rules(repo.root()).ok().as_ref() != Some(&rules) {
                guard::save_rules(repo.root(), &rules)?;
            }
        }
        Ok(())
    }

    fn save_approvals(&self, text: &str) -> Result<()> {
        let mut by_repo = vec![String::new(); self.repos.len()];
        for line in text.lines() {
            let Some((hash, path)) = line.split_once('\t') else { continue };
            let (repo, path) = self.locate(path)?;
            let ix = self.repos.iter().position(|(_, r)| std::ptr::eq(r, repo)).expect("located repo");
            by_repo[ix].push_str(&format!("{hash}\t{path}\n"));
        }
        for ((_, repo), text) in self.repos.iter().zip(by_repo) {
            if repo.read_approvals()? != text {
                repo.write_approvals(&text)?;
            }
        }
        Ok(())
    }

    fn watch(&self, on_change: Box<dyn Fn() + Send + Sync>) -> Result<Box<dyn Any + Send>> {
        let on_change: std::sync::Arc<dyn Fn() + Send + Sync> = on_change.into();
        let mut watchers = Vec::new();
        for (_, repo) in &self.repos {
            let on_change = on_change.clone();
            watchers.push(crate::watch(repo, move || on_change())?);
        }
        if watchers.is_empty() {
            bail!("nothing to watch");
        }
        Ok(Box::new(watchers))
    }
}
