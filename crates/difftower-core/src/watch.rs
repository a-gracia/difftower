use std::{path::Path, time::Duration};

use anyhow::Result;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use notify_debouncer_mini::{
    DebounceEventResult, Debouncer, new_debouncer, notify::RecommendedWatcher,
    notify::RecursiveMode,
};

use crate::Repo;

/// Keeps the watch alive; dropping it stops watching.
pub struct RepoWatcher {
    _debouncer: Debouncer<RecommendedWatcher>,
}

/// Calls `on_change` after the working tree or HEAD settles following a burst
/// of edits. Ignored paths are skipped, so builds writing to `target/` or
/// `node_modules/` stay quiet.
pub fn watch(repo: &Repo, on_change: impl Fn() + Send + 'static) -> Result<RepoWatcher> {
    let root = repo.root().to_path_buf();
    let watch_root = root.clone();
    let git_dir = repo.git_dir().to_path_buf();
    let ignore = load_ignore(&root, &git_dir);
    let mut debouncer = new_debouncer(Duration::from_millis(150), move |res: DebounceEventResult| {
        let Ok(events) = res else { return };
        let relevant = events.iter().any(|event| {
            let path = &event.path;
            if let Ok(rel) = path.strip_prefix(&git_dir) {
                // Commits and checkouts move HEAD; everything else under
                // .git, including Difftower's own index, is noise.
                return rel == Path::new("HEAD")
                    || rel == Path::new("packed-refs")
                    || rel.starts_with("refs/heads");
            }
            path.strip_prefix(&root).is_ok_and(|rel| {
                !ignore.matched_path_or_any_parents(rel, path.is_dir()).is_ignore()
            })
        });
        if relevant {
            on_change();
        }
    })?;
    debouncer.watcher().watch(&watch_root, RecursiveMode::Recursive)?;
    Ok(RepoWatcher { _debouncer: debouncer })
}

fn load_ignore(root: &Path, git_dir: &Path) -> Gitignore {
    let mut builder = GitignoreBuilder::new(root);
    builder.add(root.join(".gitignore"));
    builder.add(git_dir.join("info/exclude"));
    builder.build().unwrap_or_else(|_| Gitignore::empty())
}
