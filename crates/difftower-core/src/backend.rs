use std::{any::Any, path::Path};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::{
    Change, Repo, guard,
    multi::Multi,
    testing::{self, Runner, TestEvent, TestSuite, TestTarget},
};

/// Reports a test run's progress; see [`Backend::run_tests`].
pub type OnTestEvent = Box<dyn Fn(TestEvent) + Send + Sync>;

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
    /// The tests of every repository, one suite per runner. Slow: runners
    /// start to list them.
    fn tests(&self) -> Result<Vec<TestSuite>>;
    /// Starts running `target` of the `runner` suite in `repo`, reporting
    /// through `on_event`, with paths and ids as [`Self::tests`] gives them.
    /// Dropping the returned handle stops the run.
    fn run_tests(
        &self,
        repo: &str,
        runner: Runner,
        target: TestTarget,
        on_event: OnTestEvent,
    ) -> Result<Box<dyn Any + Send>>;
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

    fn tests(&self) -> Result<Vec<TestSuite>> {
        Ok(repo_tests(self.repo.root(), ""))
    }

    fn run_tests(
        &self,
        repo: &str,
        runner: Runner,
        target: TestTarget,
        on_event: OnTestEvent,
    ) -> Result<Box<dyn Any + Send>> {
        if !repo.is_empty() {
            anyhow::bail!("no repository {repo}");
        }
        run_repo_tests(self.repo.root(), "", runner, target, on_event)
    }

    fn watch(&self, on_change: Box<dyn Fn() + Send + Sync>) -> Result<Box<dyn Any + Send>> {
        Ok(Box::new(crate::watch(&self.repo, on_change)?))
    }
}

/// The test suites of the repository at `root`, paths placed under `name`.
pub(crate) fn repo_tests(root: &Path, name: &str) -> Vec<TestSuite> {
    let Ok(runners) = testing::runners(root) else {
        // An invalid configuration file is reported with the rules.
        return Vec::new();
    };
    runners
        .into_iter()
        .map(|(runner, command)| match testing::discover(root, runner, &command) {
            Ok(tests) => TestSuite {
                repo: name.to_owned(),
                runner,
                tests: tests
                    .into_iter()
                    .map(|t| testing::TestCase { id: join(name, &t.id), file: join(name, &t.file), ..t })
                    .collect(),
                error: None,
            },
            Err(err) => TestSuite { repo: name.to_owned(), runner, tests: Vec::new(), error: Some(format!("{err:#}")) },
        })
        .collect()
}

/// Runs tests of the repository at `root`, whose paths [`repo_tests`]
/// placed under `name`.
pub(crate) fn run_repo_tests(
    root: &Path,
    name: &str,
    runner: Runner,
    target: TestTarget,
    on_event: OnTestEvent,
) -> Result<Box<dyn Any + Send>> {
    let (_, command) = testing::runners(root)?
        .into_iter()
        .find(|(r, _)| *r == runner)
        .ok_or_else(|| anyhow::anyhow!("{} does not use {}", dir_name(&root.display().to_string()), runner.label()))?;
    let unprefix = |path: &str| -> String {
        if name.is_empty() {
            return path.to_owned();
        }
        path.strip_prefix(name).and_then(|p| p.strip_prefix('/')).unwrap_or(path).to_owned()
    };
    let target = match target {
        TestTarget::All => TestTarget::All,
        TestTarget::Path { path } => TestTarget::Path { path: unprefix(&path) },
        TestTarget::Case { id, line } => TestTarget::Case { id: unprefix(&id), line },
    };
    let prefix = name.to_owned();
    let on_event: OnTestEvent = Box::new(move |event| {
        on_event(match event {
            TestEvent::Result { id, outcome } => TestEvent::Result { id: join(&prefix, &id), outcome },
            event => event,
        })
    });
    Ok(Box::new(testing::run(root, runner, &command, &target, on_event)?))
}

/// `path` under the directory `name`, or as is for none.
pub(crate) fn join(name: &str, path: &str) -> String {
    if name.is_empty() { path.to_owned() } else { format!("{name}/{path}") }
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
    Ok(Status { changes, files, rules, rules_error })
}

pub(crate) fn dir_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_owned())
}
