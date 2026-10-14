use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context as _, Result, bail};

/// A git tree object id.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TreeId(pub String);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeStatus {
    Added,
    Deleted,
    Modified,
    Renamed,
    TypeChanged,
}

/// One file that differs between the baseline and the working tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub status: ChangeStatus,
    /// Path relative to the repository root, after any rename.
    pub path: String,
    /// The path in the baseline, for renames.
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
/// refs and stash are never touched.
#[derive(Clone, Debug)]
pub struct Repo {
    root: PathBuf,
    git_dir: PathBuf,
    index: PathBuf,
}

impl Repo {
    pub fn discover(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let root = git_output(path, &["rev-parse", "--show-toplevel"], None)
            .with_context(|| format!("{} is not inside a git repository", path.display()))?;
        let root = PathBuf::from(root.trim_end());
        let git_dir = PathBuf::from(
            git_output(&root, &["rev-parse", "--absolute-git-dir"], None)?.trim_end(),
        );
        let dt_dir = git_dir.join("difftower");
        std::fs::create_dir_all(&dt_dir)?;
        let index = dt_dir.join("index");
        if !index.exists() {
            // Seeding from the real index reuses git's stat cache, so the
            // first snapshot does not rehash every file.
            let real = PathBuf::from(
                git_output(&root, &["rev-parse", "--path-format=absolute", "--git-path", "index"], None)?
                    .trim_end(),
            );
            if real.exists() {
                std::fs::copy(&real, &index)?;
            }
        }
        Ok(Self { root, git_dir, index })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    fn git(&self, args: &[&str]) -> Result<String> {
        git_output(&self.root, args, Some(&self.index))
    }

    /// Records the whole working tree, untracked files included and ignored
    /// files excluded, as a tree object.
    pub fn snapshot(&self) -> Result<TreeId> {
        self.git(&["add", "--all", "--", "."])?;
        Ok(TreeId(self.git(&["write-tree"])?.trim_end().to_owned()))
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

    /// Everything that changed in the working tree since `baseline`.
    pub fn changes(&self, baseline: &TreeId) -> Result<Changes> {
        let current = self.snapshot()?;
        if current == *baseline {
            return Ok(Changes::default());
        }
        let names = self.git(&[
            "diff-tree", "-r", "-z", "-M", "--name-status", "--no-ext-diff", &baseline.0, &current.0,
        ])?;
        let patch = self.git(&[
            "diff-tree", "-r", "-M", "-p", "--no-color", "--no-ext-diff", "--full-index", &baseline.0,
            &current.0,
        ])?;
        let mut files = parse_name_status(&names)?;
        let sections = split_patch(&patch);
        if sections.len() != files.len() {
            bail!("git reported {} files but {} patch sections", files.len(), sections.len());
        }
        for (file, section) in files.iter_mut().zip(sections) {
            file.patch = section.to_owned();
        }
        Ok(Changes { files })
    }

    /// Every file in the working tree as of the last snapshot, ignored files
    /// excluded, sorted by path.
    pub fn files(&self) -> Result<Vec<String>> {
        let out = self.git(&["ls-files", "-z", "--cached"])?;
        Ok(out.split('\0').filter(|f| !f.is_empty()).map(str::to_owned).collect())
    }

    /// Restores one changed file to its baseline state on disk.
    pub fn revert(&self, baseline: &TreeId, change: &Change) -> Result<()> {
        match change.status {
            ChangeStatus::Added => self.remove(&change.path),
            ChangeStatus::Renamed => {
                self.remove(&change.path)?;
                self.restore(baseline, change.old_path.as_deref().unwrap_or(&change.path))
            }
            _ => self.restore(baseline, &change.path),
        }
    }

    fn restore(&self, baseline: &TreeId, path: &str) -> Result<()> {
        self.git(&["restore", "--source", &baseline.0, "--worktree", "--", path])?;
        Ok(())
    }

    fn remove(&self, path: &str) -> Result<()> {
        match std::fs::remove_file(self.root.join(path)) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err.into()),
            _ => Ok(()),
        }
    }
}

fn git_output(cwd: &Path, args: &[&str], index: Option<&Path>) -> Result<String> {
    let mut cmd = Command::new("git");
    cmd.current_dir(cwd)
        .args(["-c", "core.quotePath=false"])
        .args(args)
        .stdin(Stdio::null())
        .env("GIT_OPTIONAL_LOCKS", "0");
    if let Some(index) = index {
        cmd.env("GIT_INDEX_FILE", index);
    }
    let out = cmd.output().context("failed to run git")?;
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
