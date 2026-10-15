use std::{
    collections::BTreeSet,
    io::Write as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};

/// A git tree object id.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TreeId(pub String);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChangeStatus {
    Added,
    Deleted,
    Modified,
    Renamed,
    TypeChanged,
}

/// One file that differs between the base tree and the working tree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    pub status: ChangeStatus,
    /// Path relative to the repository root, after any rename.
    pub path: String,
    /// The path in the base tree, for renames.
    pub old_path: Option<String>,
    /// This file's section of the unified patch.
    pub patch: String,
}

#[derive(Clone, Debug, Default)]
pub struct Changes {
    pub files: Vec<Change>,
}

impl Changes {
    pub fn patch(&self) -> String {
        self.files.iter().map(|c| c.patch.as_str()).collect()
    }
}

/// A git working tree observed by Difftower.
///
/// Snapshots are written through a private index file so the user's index,
/// refs and stash are never touched. Clones share one snapshot state, so a
/// repository has a single snapshot in progress at a time.
#[derive(Clone, Debug)]
pub struct Repo {
    root: PathBuf,
    git_dir: PathBuf,
    index: PathBuf,
    state: Arc<Mutex<SnapshotState>>,
    git_calls: Arc<AtomicUsize>,
}

#[derive(Debug, Default)]
struct SnapshotState {
    /// Watchers currently reporting changed paths through [`Repo::mark_dirty`].
    watchers: usize,
    /// Paths changed since the last snapshot. `None` means unknown, so the
    /// next snapshot adds everything; that is always the case unwatched.
    dirty: Option<BTreeSet<String>>,
    /// The last snapshot.
    tree: Option<TreeId>,
    /// Files listed for a snapshot.
    files: Option<(TreeId, Vec<String>)>,
    /// Changes computed for a base, path prefix and snapshot.
    changes: Option<(TreeId, String, TreeId, Changes)>,
}

impl Repo {
    pub fn discover(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if !path.is_dir() {
            bail!("{} does not exist or is not a directory", path.display());
        }
        let root = git_output(path, &["rev-parse", "--show-toplevel"], None, None)
            .with_context(|| format!("{} is not inside a git repository", path.display()))?;
        let root = PathBuf::from(root.trim_end());
        let git_dir = PathBuf::from(
            git_output(&root, &["rev-parse", "--absolute-git-dir"], None, None)?.trim_end(),
        );
        let dt_dir = git_dir.join("difftower");
        std::fs::create_dir_all(&dt_dir)?;
        let index = dt_dir.join("index");
        if !index.exists() {
            // Seeding from the real index reuses git's stat cache, so the
            // first snapshot does not rehash every file.
            let real = PathBuf::from(
                git_output(&root, &["rev-parse", "--path-format=absolute", "--git-path", "index"], None, None)?
                    .trim_end(),
            );
            if real.exists() {
                std::fs::copy(&real, &index)?;
            }
        }
        Ok(Self { root, git_dir, index, state: Arc::default(), git_calls: Arc::default() })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    fn git(&self, args: &[&str]) -> Result<String> {
        self.git_calls.fetch_add(1, Ordering::Relaxed);
        git_output(&self.root, args, Some(&self.index), None)
    }

    /// How many git processes this repository has run, for tests that guard
    /// against refresh loops.
    #[doc(hidden)]
    pub fn git_calls(&self) -> usize {
        self.git_calls.load(Ordering::Relaxed)
    }

    /// Records the whole working tree, untracked files included and ignored
    /// files excluded, as a tree object.
    ///
    /// While a watcher reports changed paths, only those are re-added, and
    /// with none the previous tree is returned without running git.
    pub fn snapshot(&self) -> Result<TreeId> {
        let mut state = self.state.lock().unwrap();
        let dirty = state.dirty.take();
        let result = self.write_snapshot(dirty.as_ref(), state.tree.as_ref());
        match &result {
            Ok(tree) => {
                state.tree = Some(tree.clone());
                state.dirty = (state.watchers > 0).then(BTreeSet::new);
            }
            Err(_) => state.dirty = None,
        }
        result
    }

    fn write_snapshot(&self, dirty: Option<&BTreeSet<String>>, last: Option<&TreeId>) -> Result<TreeId> {
        match (dirty, last) {
            (Some(paths), Some(tree)) if paths.is_empty() => return Ok(tree.clone()),
            // A path that vanished before git saw it fails the pathspec;
            // adding everything is always right.
            (Some(paths), Some(_)) if self.add_paths(paths).is_ok() => {}
            _ => {
                self.git(&["add", "--all", "--", "."])?;
            }
        }
        Ok(TreeId(self.git(&["write-tree"])?.trim_end().to_owned()))
    }

    fn add_paths(&self, paths: &BTreeSet<String>) -> Result<()> {
        self.git_calls.fetch_add(1, Ordering::Relaxed);
        let list: Vec<u8> = paths.iter().flat_map(|p| p.bytes().chain([0])).collect();
        git_output(
            &self.root,
            &["--literal-pathspecs", "add", "--all", "--pathspec-from-file=-", "--pathspec-file-nul"],
            Some(&self.index),
            Some(&list),
        )?;
        Ok(())
    }

    /// Starts accepting changed paths from a watcher; the next snapshot still
    /// adds everything, since earlier edits were not seen.
    pub(crate) fn start_tracking(&self) {
        let mut state = self.state.lock().unwrap();
        state.watchers += 1;
        state.dirty = None;
    }

    pub(crate) fn stop_tracking(&self) {
        let mut state = self.state.lock().unwrap();
        state.watchers -= 1;
        state.dirty = None;
    }

    /// Notes paths, relative to the root, that changed on disk.
    pub(crate) fn mark_dirty(&self, paths: impl IntoIterator<Item = String>) {
        if let Some(dirty) = &mut self.state.lock().unwrap().dirty {
            dirty.extend(paths);
        }
    }

    /// Forgets which paths changed, so the next snapshot adds everything.
    pub(crate) fn mark_all_dirty(&self) {
        self.state.lock().unwrap().dirty = None;
    }

    /// The tree of the current commit, or the empty tree before the first commit.
    pub fn head_tree(&self) -> Result<TreeId> {
        let id = match self.git(&["rev-parse", "--verify", "--quiet", "HEAD^{tree}"]) {
            Ok(id) => id,
            // stdin is empty, so this hashes the empty tree without writing it.
            Err(_) => self.git(&["hash-object", "-t", "tree", "--stdin"])?,
        };
        Ok(TreeId(id.trim_end().to_owned()))
    }

    /// Everything in the working tree that differs from `base`.
    pub fn changes(&self, base: &TreeId) -> Result<Changes> {
        self.changes_under(base, "")
    }

    /// Like [`Self::changes`], with every path, in the patches too, placed
    /// under the directory `prefix` (no trailing slash; empty for none).
    pub fn changes_under(&self, base: &TreeId, prefix: &str) -> Result<Changes> {
        let current = self.snapshot()?;
        if current == *base {
            return Ok(Changes::default());
        }
        if let Some((b, p, c, changes)) = &self.state.lock().unwrap().changes
            && (b, p.as_str(), c) == (base, prefix, &current)
        {
            return Ok(changes.clone());
        }
        let changes = self.diff(base, &current, prefix)?;
        self.state.lock().unwrap().changes = Some((base.clone(), prefix.to_owned(), current, changes.clone()));
        Ok(changes)
    }

    fn diff(&self, base: &TreeId, current: &TreeId, prefix: &str) -> Result<Changes> {
        let names = self.git(&[
            "diff-tree", "-r", "-z", "-M", "--name-status", "--no-ext-diff", &base.0, &current.0,
        ])?;
        let dir = if prefix.is_empty() { String::new() } else { format!("{prefix}/") };
        let (src_prefix, dst_prefix) = (format!("--src-prefix=a/{dir}"), format!("--dst-prefix=b/{dir}"));
        let patch = self.git(&[
            "diff-tree", "-r", "-M", "-p", "--no-color", "--no-ext-diff", "--full-index", &src_prefix,
            &dst_prefix, &base.0, &current.0,
        ])?;
        let mut files = parse_name_status(&names)?;
        let sections = split_patch(&patch);
        if sections.len() != files.len() {
            bail!("git reported {} files but {} patch sections", files.len(), sections.len());
        }
        for (file, section) in files.iter_mut().zip(sections) {
            file.patch = prefix_rename_lines(section, &dir);
            file.path.insert_str(0, &dir);
            if let Some(old) = &mut file.old_path {
                old.insert_str(0, &dir);
            }
        }
        Ok(Changes { files })
    }

    /// One changed file's patch with the whole file as context, so it can be
    /// read in full, its paths placed under `prefix` like
    /// [`Self::changes_under`]. `change` uses this repository's own paths.
    pub fn file_patch(&self, base: &TreeId, change: &Change, prefix: &str) -> Result<String> {
        let current = self.snapshot()?;
        let dir = if prefix.is_empty() { String::new() } else { format!("{prefix}/") };
        let (src_prefix, dst_prefix) = (format!("--src-prefix=a/{dir}"), format!("--dst-prefix=b/{dir}"));
        let mut args = vec![
            "--literal-pathspecs", "diff-tree", "-r", "-M", "-p", "--no-color", "--no-ext-diff", "--full-index",
            // Context larger than any file: every line of it.
            "--unified=100000000", &src_prefix, &dst_prefix, &base.0, &current.0, "--",
        ];
        args.extend(change.old_path.iter().map(String::as_str));
        args.push(&change.path);
        let patch = self.git(&args)?;
        Ok(prefix_rename_lines(&patch, &dir))
    }

    /// Accepted changes to protected files, as stored by [`crate::Approvals::to_text`].
    pub fn read_approvals(&self) -> Result<String> {
        match std::fs::read_to_string(self.approvals_path()) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
            result => Ok(result?),
        }
    }

    pub fn write_approvals(&self, text: &str) -> Result<()> {
        Ok(std::fs::write(self.approvals_path(), text)?)
    }

    fn approvals_path(&self) -> PathBuf {
        self.git_dir.join("difftower").join("approvals")
    }

    /// Every file in the working tree as of the last snapshot, ignored files
    /// excluded, sorted by path.
    pub fn files(&self) -> Result<Vec<String>> {
        let last = self.state.lock().unwrap().tree.clone();
        let tree = match last {
            Some(tree) => tree,
            None => self.snapshot()?,
        };
        if let Some((t, files)) = &self.state.lock().unwrap().files
            && *t == tree
        {
            return Ok(files.clone());
        }
        let out = self.git(&["ls-files", "-z", "--cached"])?;
        let files: Vec<String> = out.split('\0').filter(|f| !f.is_empty()).map(str::to_owned).collect();
        self.state.lock().unwrap().files = Some((tree, files.clone()));
        Ok(files)
    }

    /// Restores one changed file to its state in `base`.
    pub fn revert(&self, base: &TreeId, change: &Change) -> Result<()> {
        // Seen by the next snapshot even before the watcher reports it.
        self.mark_dirty(change.old_path.iter().chain([&change.path]).cloned());
        match change.status {
            ChangeStatus::Added => self.remove(&change.path),
            ChangeStatus::Renamed => {
                self.remove(&change.path)?;
                self.restore(base, change.old_path.as_deref().unwrap_or(&change.path))
            }
            _ => self.restore(base, &change.path),
        }
    }

    fn restore(&self, base: &TreeId, path: &str) -> Result<()> {
        self.git(&["restore", "--source", &base.0, "--worktree", "--", path])?;
        Ok(())
    }

    fn remove(&self, path: &str) -> Result<()> {
        match std::fs::remove_file(self.root.join(path)) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err.into()),
            _ => Ok(()),
        }
    }
}

fn git_output(cwd: &Path, args: &[&str], index: Option<&Path>, input: Option<&[u8]>) -> Result<String> {
    let mut cmd = Command::new("git");
    cmd.current_dir(cwd)
        .args(["-c", "core.quotePath=false"])
        .args(args)
        .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("GIT_OPTIONAL_LOCKS", "0");
    if let Some(index) = index {
        cmd.env("GIT_INDEX_FILE", index);
    }
    let mut child = cmd.spawn().context("failed to run git")?;
    if let Some(input) = input {
        child.stdin.take().expect("piped").write_all(input)?;
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Parses `git diff-tree -z --name-status` output.
fn parse_name_status(out: &str) -> Result<Vec<Change>> {
    let mut fields = out.split('\0').filter(|f| !f.is_empty());
    let mut files = Vec::new();
    while let Some(code) = fields.next() {
        let next = |fields: &mut dyn Iterator<Item = &str>| {
            fields.next().map(str::to_owned).context("truncated git name-status output")
        };
        let (status, old_path, path) = match code.as_bytes()[0] {
            b'A' => (ChangeStatus::Added, None, next(&mut fields)?),
            b'D' => (ChangeStatus::Deleted, None, next(&mut fields)?),
            b'M' => (ChangeStatus::Modified, None, next(&mut fields)?),
            b'T' => (ChangeStatus::TypeChanged, None, next(&mut fields)?),
            b'R' => {
                let old = next(&mut fields)?;
                (ChangeStatus::Renamed, Some(old), next(&mut fields)?)
            }
            other => bail!("unexpected git status {:?}", other as char),
        };
        files.push(Change { status, path, old_path, patch: String::new() });
    }
    Ok(files)
}

/// Adds `dir` to the paths of `rename from`/`rename to` header lines, which
/// git writes without the `--src-prefix`/`--dst-prefix` it uses elsewhere.
fn prefix_rename_lines(section: &str, dir: &str) -> String {
    if dir.is_empty() {
        return section.to_owned();
    }
    let mut out = String::with_capacity(section.len() + 2 * dir.len());
    let mut in_header = true;
    for line in section.split_inclusive('\n') {
        in_header &= !line.starts_with("@@");
        let key = ["rename from ", "rename to ", "copy from ", "copy to "]
            .into_iter()
            .find(|key| in_header && line.starts_with(key));
        match key {
            Some(key) => {
                let path = &line[key.len()..];
                match path.strip_prefix('"') {
                    Some(quoted) => out.push_str(&format!("{key}\"{dir}{quoted}")),
                    None => out.push_str(&format!("{key}{dir}{path}")),
                }
            }
            None => out.push_str(line),
        }
    }
    out
}

/// Splits a multi-file patch at each `diff --git` header.
fn split_patch(patch: &str) -> Vec<&str> {
    let mut starts: Vec<usize> = Vec::new();
    let mut offset = 0;
    for line in patch.split_inclusive('\n') {
        if line.starts_with("diff --git ") {
            starts.push(offset);
        }
        offset += line.len();
    }
    starts
        .iter()
        .enumerate()
        .map(|(i, &start)| &patch[start..starts.get(i + 1).copied().unwrap_or(patch.len())])
        .collect()
}
