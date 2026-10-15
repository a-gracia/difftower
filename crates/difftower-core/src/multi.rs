//! Several repositories shown as one: the repositories side by side in a
//! directory, and the directory itself when it is a repository too. Every
//! path gets its repository's directory name as a prefix (none for the outer
//! repository); each repository keeps its own rules and approvals.

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
    /// Each repository with its directory name, sorted by name; an empty
    /// name, first, is `root` itself.
    repos: Vec<(String, Repo)>,
}

impl Multi {
    /// The repositories directly inside `root`, with their directory names.
    pub fn inner_repos(root: &Path) -> Result<Vec<(String, Repo)>> {
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
        Ok(repos)
    }

    /// `inner`, from [`Self::inner_repos`], together with `outer`, the
    /// repository at `root` itself, if any.
    pub fn new(root: &Path, outer: Option<Repo>, mut inner: Vec<(String, Repo)>) -> Self {
        inner.extend(outer.map(|repo| (String::new(), repo)));
        inner.sort_by(|a, b| a.0.cmp(&b.0));
        Self { root: root.to_path_buf(), repos: inner }
    }

    /// The repository holding `path`, and `path` relative to it.
    fn locate<'a>(&self, path: &'a str) -> Result<(&Repo, &'a str)> {
        Ok(self.locate_ix(path)?.1)
    }

    fn locate_ix<'a>(&self, path: &'a str) -> Result<(usize, (&Repo, &'a str))> {
        let inner = self.repos.iter().enumerate().find_map(|(ix, (name, repo))| {
            let rest = path.strip_prefix(name.as_str())?.strip_prefix('/')?;
            (!name.is_empty()).then_some((ix, (repo, rest)))
        });
        inner
            .or_else(|| self.outer().map(|repo| (0, (repo, path))))
            .ok_or_else(|| anyhow!("{path} is not inside any of the repositories"))
    }

    /// The repository at the root itself, if it is one.
    fn outer(&self) -> Option<&Repo> {
        self.repos.first().filter(|(name, _)| name.is_empty()).map(|(_, repo)| repo)
    }

    /// Whether `path`, of the outer repository, is one of the inner ones or
    /// inside it: the outer repository may track them as submodules or list
    /// them as untracked directories.
    fn is_inner(&self, path: &str) -> bool {
        let path = path.trim_end_matches('/');
        self.repos.iter().any(|(name, _)| {
            !name.is_empty() && path.strip_prefix(name.as_str()).is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
        })
    }
}

/// `path` under the directory `name`, or as is for the outer repository.
fn join(name: &str, path: &str) -> String {
    if name.is_empty() { path.to_owned() } else { format!("{name}/{path}") }
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

    fn repos(&self) -> Vec<String> {
        self.repos.iter().map(|(name, _)| name.clone()).collect()
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
            let outer = name.is_empty();
            status.changes.extend(repo_status.changes.into_iter().filter(|c| !(outer && self.is_inner(&c.path))));
            status
                .files
                .extend(repo_status.files.iter().filter(|f| !(outer && self.is_inner(f))).map(|f| join(name, f)));
            let dir = globset::escape(name);
            status.rules.extend(repo_status.rules.iter().map(|rule| match rule.strip_prefix('!') {
                Some(glob) => format!("!{}", join(&dir, glob)),
                None => join(&dir, rule),
            }));
            if let Some(err) = repo_status.rules_error {
                errors.push(format!("{name}: {err}"));
            }
            for line in repo_status.approvals.lines() {
                if let Some((hash, path)) = line.split_once('\t') {
                    status.approvals.push_str(&format!("{hash}\t{}\n", join(name, path)));
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

    fn file_diff(&self, change: &Change) -> Result<String> {
        let (ix, (repo, path)) = self.locate_ix(&change.path)?;
        let name = &self.repos[ix].0;
        let old_path = match &change.old_path {
            Some(old) => Some(self.locate(old)?.1.to_owned()),
            None => None,
        };
        let change = Change { path: path.to_owned(), old_path, ..change.clone() };
        repo.file_patch(&repo.head_tree()?, &change, name)
    }

    fn read_file(&self, path: &str) -> Result<Vec<u8>> {
        let (repo, path) = self.locate(path)?;
        Ok(std::fs::read(repo.root().join(path))?)
    }

    /// Splits the rules by repository, writing only the files that change.
    fn save_rules(&self, rules: &[String]) -> Result<()> {
        let mut by_repo: Vec<Vec<String>> = vec![Vec::new(); self.repos.len()];
        for rule in rules {
            let (bang, glob) = match rule.strip_prefix('!') {
                Some(glob) => ("!", glob),
                None => ("", rule.as_str()),
            };
            let inner = self.repos.iter().enumerate().find_map(|(ix, (name, _))| {
                let rest = glob.strip_prefix(&format!("{}/", globset::escape(name)))?;
                (!name.is_empty()).then_some((ix, rest))
            });
            let (ix, glob) = inner
                .or_else(|| self.outer().map(|_| (0, glob)))
                .with_context(|| format!("the rule {rule} must be inside one of the repositories"))?;
            by_repo[ix].push(format!("{bang}{glob}"));
        }
        for ((_, repo), rules) in self.repos.iter().zip(by_repo) {
            if guard::load_rules(repo.root()).ok().as_ref() != Some(&rules) {
                guard::save_rules(repo.root(), &rules)?;
                repo.mark_dirty([guard::CONFIG_FILE.to_owned()]);
            }
        }
        Ok(())
    }

    fn save_approvals(&self, text: &str) -> Result<()> {
        let mut by_repo = vec![String::new(); self.repos.len()];
        for line in text.lines() {
            let Some((hash, path)) = line.split_once('\t') else { continue };
            let (ix, (_, path)) = self.locate_ix(path)?;
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
