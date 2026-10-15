use std::{any::Any, path::Path};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::{Change, Repo, guard, multi::Multi};

/// Everything the UI shows about a repository at one moment.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Status {
    /// Everything that differs from HEAD.
    pub changes: Vec<Change>,
    /// Every file in the working tree, ignored files excluded.
    pub files: Vec<String>,
    /// Protection rules from `.difftower.toml`.
    pub rules: Vec<String>,
    /// Why the rules could not be read, if they could not.
    pub rules_error: Option<String>,
    /// Stored approvals, in [`crate::Approvals::to_text`] format.
    pub approvals: String,
}

/// A repository, on this machine or on a remote host.
pub trait Backend: Send + Sync {
    /// The repository's directory name.
    fn name(&self) -> String;
    /// The host the repository lives on, for remote backends.
    fn host(&self) -> Option<String>;
    /// Absolute path of the repository root on its host.
    fn root(&self) -> String;
    /// The directory of each repository this backend shows, relative to
    /// [`Self::root`], sorted; empty for the root itself. Paths in a status
    /// start with their repository's directory.
    fn repos(&self) -> Vec<String>;
    fn status(&self) -> Result<Status>;
    /// Restores one changed file to its state in HEAD.
    fn revert(&self, change: &Change) -> Result<()>;
    fn read_file(&self, path: &str) -> Result<Vec<u8>>;
    /// One changed file's patch with the whole file as context.
    fn file_diff(&self, change: &Change) -> Result<String>;
    fn save_rules(&self, rules: &[String]) -> Result<()>;
    fn save_approvals(&self, text: &str) -> Result<()>;
    /// Calls `on_change` whenever the working tree or HEAD changes, until
    /// the returned handle is dropped.
    fn watch(&self, on_change: Box<dyn Fn() + Send + Sync>) -> Result<Box<dyn Any + Send>>;
}

/// Opens `path` as a repository, or, when it is a directory holding
/// repositories, all of them together, along with `path` itself if it is
/// a repository too.
pub fn open_path(path: &Path) -> Result<Box<dyn Backend>> {
    match Repo::discover(path) {
        Ok(repo) => {
            let root = repo.root().to_path_buf();
            let inner = Multi::inner_repos(&root)?;
            if inner.is_empty() {
                Ok(Box::new(Local::new(repo)))
            } else {
                Ok(Box::new(Multi::new(&root, Some(repo), inner)))
            }
        }
        Err(err) if !path.is_dir() => Err(err),
        Err(err) => {
            let inner = Multi::inner_repos(path)?;
            if inner.is_empty() {
                return Err(err.context("it contains no repositories either"));
            }
            Ok(Box::new(Multi::new(path, None, inner)))
        }
    }
}

/// A repository on this machine.
pub struct Local {
    repo: Repo,
}

impl Local {
    pub fn new(repo: Repo) -> Self {
        Self { repo }
    }
}

impl Backend for Local {
    fn name(&self) -> String {
        dir_name(&self.root())
    }

    fn host(&self) -> Option<String> {
        None
    }

    fn root(&self) -> String {
        self.repo.root().display().to_string()
    }

    fn repos(&self) -> Vec<String> {
        vec![String::new()]
    }

    fn status(&self) -> Result<Status> {
        repo_status(&self.repo, "")
    }

    fn revert(&self, change: &Change) -> Result<()> {
        self.repo.revert(&self.repo.head_tree()?, change)
    }

    fn read_file(&self, path: &str) -> Result<Vec<u8>> {
        Ok(std::fs::read(self.repo.root().join(path))?)
    }

    fn file_diff(&self, change: &Change) -> Result<String> {
        self.repo.file_patch(&self.repo.head_tree()?, change, "")
    }

    fn save_rules(&self, rules: &[String]) -> Result<()> {
        guard::save_rules(self.repo.root(), rules)?;
        // Seen by the next snapshot even before the watcher reports it.
        self.repo.mark_dirty([guard::CONFIG_FILE.to_owned()]);
        Ok(())
    }

    fn save_approvals(&self, text: &str) -> Result<()> {
        self.repo.write_approvals(text)
    }

    fn watch(&self, on_change: Box<dyn Fn() + Send + Sync>) -> Result<Box<dyn Any + Send>> {
        Ok(Box::new(crate::watch(&self.repo, on_change)?))
    }
}

/// The status of one repository, its paths placed under `prefix`.
pub(crate) fn repo_status(repo: &Repo, prefix: &str) -> Result<Status> {
    let base = repo.head_tree()?;
    let changes = repo.changes_under(&base, prefix)?.files;
    let files = repo.files()?;
    let (rules, rules_error) = match guard::load_rules(repo.root()) {
        Ok(rules) => (rules, None),
        Err(err) => (Vec::new(), Some(err.to_string())),
    };
    let approvals = repo.read_approvals()?;
    Ok(Status { changes, files, rules, rules_error, approvals })
}

pub(crate) fn dir_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_owned())
}
