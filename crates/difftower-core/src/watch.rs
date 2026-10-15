//! Reports real edits and commits in a repository, and nothing else.
//!
//! A refresh runs git, which reads the working tree and `.git/HEAD` and
//! writes Difftower's index and git objects. Reacting to any of that would
//! refresh again forever, so only changes count (never reads), only `HEAD`
//! and refs count inside `.git`, ignored directories are not watched at all,
//! and a batch of events notifies only if the snapshot or HEAD moved.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, Weak, mpsc},
    time::Duration,
};

use anyhow::Result;
use ignore::{
    Match, WalkBuilder,
    gitignore::{Gitignore, GitignoreBuilder},
};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};

use crate::{Repo, TreeId};

/// Quiet time after the last event before a batch is handled.
const DEBOUNCE: Duration = Duration::from_millis(150);

/// Keeps the watch alive; dropping it stops watching.
pub struct RepoWatcher {
    _watcher: Arc<Mutex<RecommendedWatcher>>,
    repo: Repo,
}

impl Drop for RepoWatcher {
    fn drop(&mut self) {
        self.repo.stop_tracking();
    }
}

/// Calls `on_change` when the working tree or HEAD changes, at most once
/// per burst of edits.
pub fn watch(repo: &Repo, on_change: impl Fn() + Send + 'static) -> Result<RepoWatcher> {
    let (tx, rx) = mpsc::channel();
    let watcher = Arc::new(Mutex::new(notify::recommended_watcher(tx)?));
    let (ignores, dirs) = Ignores::load(repo.root(), repo.git_dir());
    {
        let mut watcher = watcher.lock().unwrap();
        for dir in &dirs {
            // A directory can vanish between the walk and here.
            let _ = watcher.watch(dir, RecursiveMode::NonRecursive);
        }
        watcher.watch(repo.git_dir(), RecursiveMode::NonRecursive)?;
        let refs = repo.git_dir().join("refs");
        if refs.is_dir() {
            watcher.watch(&refs, RecursiveMode::Recursive)?;
        }
    }
    repo.start_tracking();
    let last = fingerprint(repo);
    let state = Loop { repo: repo.clone(), watcher: Arc::downgrade(&watcher), ignores, last };
    std::thread::Builder::new()
        .name("difftower-watch".into())
        .spawn(move || state.run(rx, on_change))?;
    Ok(RepoWatcher { _watcher: watcher, repo: repo.clone() })
}

/// The snapshot and HEAD: a batch of events that leaves both unchanged,
/// such as saving a file without editing it, is not reported.
fn fingerprint(repo: &Repo) -> Option<(TreeId, TreeId)> {
    Some((repo.snapshot().ok()?, repo.head_tree().ok()?))
}

struct Loop {
    repo: Repo,
    watcher: Weak<Mutex<RecommendedWatcher>>,
    ignores: Ignores,
    last: Option<(TreeId, TreeId)>,
}

#[derive(Default)]
struct Batch {
    relevant: bool,
    /// Changed paths relative to the root; `None` once an event was lost.
    dirty: Option<Vec<String>>,
    new_dirs: Vec<PathBuf>,
    gitignore_changed: bool,
}

impl Loop {
    fn run(mut self, rx: mpsc::Receiver<notify::Result<Event>>, on_change: impl Fn()) {
        // Ends when the watcher, which owns the sender, is dropped.
        while let Ok(first) = rx.recv() {
            let mut batch = Batch { dirty: Some(Vec::new()), ..Batch::default() };
            self.classify(first, &mut batch);
            loop {
                match rx.recv_timeout(DEBOUNCE) {
                    Ok(event) => self.classify(event, &mut batch),
                    Err(mpsc::RecvTimeoutError::Timeout) => break,
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                }
            }
            if !batch.relevant {
                continue;
            }
            if batch.gitignore_changed {
                let (ignores, dirs) = Ignores::load(self.repo.root(), self.repo.git_dir());
                self.ignores = ignores;
                self.watch_dirs(&dirs);
            }
            for dir in &batch.new_dirs {
                let dirs = Ignores::walk_dirs(dir);
                self.watch_dirs(&dirs);
            }
            match batch.dirty {
                Some(paths) => self.repo.mark_dirty(paths),
                None => self.repo.mark_all_dirty(),
            }
            let now = fingerprint(&self.repo);
            if now != self.last {
                self.last = now;
                on_change();
            }
        }
    }

    fn watch_dirs(&self, dirs: &[PathBuf]) {
        let Some(watcher) = self.watcher.upgrade() else { return };
        let mut watcher = watcher.lock().unwrap();
        for dir in dirs {
            let _ = watcher.watch(dir, RecursiveMode::NonRecursive);
        }
    }

    fn classify(&self, event: notify::Result<Event>, batch: &mut Batch) {
        let Ok(event) = event else {
            // An error may mean dropped events: start over from scratch.
            batch.relevant = true;
            batch.dirty = None;
            return;
        };
        if event.need_rescan() {
            batch.relevant = true;
            batch.dirty = None;
        }
        // Reads, including git's own, are not changes.
        if matches!(event.kind, EventKind::Access(_)) {
            return;
        }
        let root = self.repo.root();
        let git_dir = self.repo.git_dir();
        for path in &event.paths {
            if let Ok(rel) = path.strip_prefix(git_dir) {
                // Commits, checkouts and branch moves; never Difftower's
                // index under .git/difftower or the objects it writes.
                if is_ref_change(rel) {
                    batch.relevant = true;
                }
                continue;
            }
            let Ok(rel) = path.strip_prefix(root) else { continue };
            let is_dir = path.is_dir();
            if rel.as_os_str().is_empty() || self.ignores.is_ignored(path, is_dir) {
                continue;
            }
            batch.relevant = true;
            if rel.file_name().is_some_and(|name| name == ".gitignore") {
                batch.gitignore_changed = true;
            }
            if is_dir && matches!(event.kind, EventKind::Create(_)) {
                batch.new_dirs.push(path.clone());
            }
            if let (Some(dirty), Some(rel)) = (&mut batch.dirty, rel.to_str()) {
                dirty.push(rel.to_owned());
            } else {
                batch.dirty = None;
            }
        }
    }
}

/// Whether a path inside `.git` records where HEAD points.
fn is_ref_change(rel: &Path) -> bool {
    rel == Path::new("HEAD")
        || rel == Path::new("packed-refs")
        || (rel.starts_with("refs") && !rel.starts_with("refs/difftower"))
}

/// The repository's ignore rules: every `.gitignore` outside ignored
/// directories, `.git/info/exclude` and the user's global excludes file.
struct Ignores {
    root: PathBuf,
    per_dir: HashMap<PathBuf, Gitignore>,
    global: Gitignore,
}

impl Ignores {
    /// Loads the rules and lists the directories worth watching.
    fn load(root: &Path, git_dir: &Path) -> (Self, Vec<PathBuf>) {
        let mut per_dir = HashMap::new();
        let mut dirs = Vec::new();
        for entry in walker(root).build().flatten() {
            let path = entry.path();
            if entry.file_type().is_some_and(|t| t.is_dir()) {
                dirs.push(path.to_path_buf());
            } else if entry.file_name() == ".gitignore"
                && let Some(dir) = path.parent()
            {
                let mut builder = GitignoreBuilder::new(dir);
                builder.add(path);
                if let Ok(gitignore) = builder.build() {
                    per_dir.insert(dir.to_path_buf(), gitignore);
                }
            }
        }
        let mut global = GitignoreBuilder::new(root);
        global.add(git_dir.join("info").join("exclude"));
        if let Some(path) = global_excludes_file() {
            global.add(path);
        }
        let global = global.build().unwrap_or_else(|_| Gitignore::empty());
        (Self { root: root.to_path_buf(), per_dir, global }, dirs)
    }

    /// The directories under `dir`, itself included, that are not ignored.
    fn walk_dirs(dir: &Path) -> Vec<PathBuf> {
        walker(dir)
            .parents(true)
            .build()
            .flatten()
            .filter(|e| e.file_type().is_some_and(|t| t.is_dir()))
            .map(|e| e.into_path())
            .collect()
    }

    /// Whether `path` or any directory above it, up to the root, is ignored.
    fn is_ignored(&self, path: &Path, is_dir: bool) -> bool {
        let Ok(rel) = path.strip_prefix(&self.root) else { return false };
        let mut current = self.root.clone();
        let mut components = rel.components().peekable();
        while let Some(component) = components.next() {
            current.push(component);
            let last = components.peek().is_none();
            if self.matches(&current, !last || is_dir) {
                return true;
            }
        }
        false
    }

    /// Git's precedence: the nearest `.gitignore` with a matching rule
    /// decides, then the repository and global excludes.
    fn matches(&self, path: &Path, is_dir: bool) -> bool {
        for dir in path.ancestors().skip(1) {
            if !dir.starts_with(&self.root) {
                break;
            }
            if let Some(gitignore) = self.per_dir.get(dir) {
                match gitignore.matched(path, is_dir) {
                    Match::Ignore(_) => return true,
                    Match::Whitelist(_) => return false,
                    Match::None => {}
                }
            }
        }
        self.global.matched(path, is_dir).is_ignore()
    }
}

/// Walks a tree the way git sees it: ignored entries and `.git` skipped,
/// hidden files kept.
fn walker(dir: &Path) -> WalkBuilder {
    let mut builder = WalkBuilder::new(dir);
    builder
        .hidden(false)
        .parents(false)
        .require_git(false)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(true)
        .filter_entry(|entry| entry.file_name() != ".git");
    builder
}

/// `core.excludesFile`, or git's default `$XDG_CONFIG_HOME/git/ignore`.
fn global_excludes_file() -> Option<PathBuf> {
    let configured = std::process::Command::new("git")
        .args(["config", "--path", "core.excludesFile"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))
        .filter(|path| !path.as_os_str().is_empty());
    configured.or_else(|| {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
        Some(config.join("git").join("ignore"))
    })
}
