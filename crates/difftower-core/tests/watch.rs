//! The watcher must report real edits and commits, and nothing else: a
//! refresh writes Difftower's own index and git objects, and a reaction to
//! those would refresh again forever.

use std::{
    fs,
    path::Path,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread::sleep,
    time::Duration,
};

use difftower_core::{Repo, RepoWatcher, watch};

/// Longer than the watcher's debounce, so pending events have been delivered.
const SETTLE: Duration = Duration::from_millis(600);

fn git(dir: &Path, args: &[&str]) {
    assert!(Command::new("git").current_dir(dir).args(args).status().unwrap().success());
}

/// A repository like a Rails or Node app: a .gitignore for logs, temp files
/// and dependencies, and an uncommitted edit so refreshes produce a diff.
fn fixture() -> (tempfile::TempDir, Repo) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git(root, &["init", "-q"]);
    git(root, &["config", "user.email", "t@t"]);
    git(root, &["config", "user.name", "t"]);
    for ignored in ["log", "tmp", "node_modules/pkg"] {
        fs::create_dir_all(root.join(ignored)).unwrap();
    }
    fs::write(root.join(".gitignore"), "/log/*\n/tmp/*\nnode_modules/\n").unwrap();
    fs::write(root.join("app.rb"), "puts 1\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "init"]);
    fs::write(root.join("app.rb"), "puts 2\n").unwrap();
    let repo = Repo::discover(root).unwrap();
    (dir, repo)
}

fn counting_watch(repo: &Repo) -> (RepoWatcher, Arc<AtomicUsize>) {
    let count = Arc::new(AtomicUsize::new(0));
    let counter = count.clone();
    let watcher = watch(repo, move || {
        counter.fetch_add(1, Ordering::SeqCst);
    })
    .unwrap();
    sleep(SETTLE);
    count.store(0, Ordering::SeqCst);
    (watcher, count)
}

/// Everything a refresh does: snapshot, diff against HEAD, list files.
fn refresh(repo: &Repo) {
    let head = repo.head_tree().unwrap();
    repo.changes(&head).unwrap();
    repo.files().unwrap();
}

#[test]
fn refreshing_does_not_trigger_the_watcher() {
    let (_dir, repo) = fixture();
    let (_watcher, count) = counting_watch(&repo);
    for _ in 0..10 {
        refresh(&repo);
    }
    sleep(SETTLE);
    assert_eq!(count.load(Ordering::SeqCst), 0, "a refresh caused change notifications");
}

#[test]
fn ignored_paths_do_not_trigger_the_watcher() {
    let (dir, repo) = fixture();
    let (_watcher, count) = counting_watch(&repo);
    for i in 0..20 {
        fs::write(dir.path().join("log/development.log"), format!("request {i}\n")).unwrap();
        fs::write(dir.path().join("tmp/cache"), format!("{i}")).unwrap();
        fs::write(dir.path().join("node_modules/pkg/index.js"), format!("// {i}")).unwrap();
        sleep(Duration::from_millis(20));
    }
    sleep(SETTLE);
    assert_eq!(count.load(Ordering::SeqCst), 0, "writes to ignored paths caused notifications");
}

#[test]
fn edits_and_commits_trigger_the_watcher() {
    let (dir, repo) = fixture();
    let (_watcher, count) = counting_watch(&repo);

    fs::write(dir.path().join("app.rb"), "puts 3\n").unwrap();
    sleep(SETTLE);
    assert!(count.swap(0, Ordering::SeqCst) > 0, "an edit was not reported");

    git(dir.path(), &["commit", "-qam", "agent"]);
    sleep(SETTLE);
    assert!(count.swap(0, Ordering::SeqCst) > 0, "a commit was not reported");
}

#[test]
fn saving_a_file_without_changing_it_is_not_reported() {
    let (dir, repo) = fixture();
    let (_watcher, count) = counting_watch(&repo);
    fs::write(dir.path().join("app.rb"), "puts 2\n").unwrap();
    sleep(SETTLE);
    assert_eq!(count.load(Ordering::SeqCst), 0, "an unchanged tree was reported");
}

#[test]
fn nested_gitignore_files_are_respected() {
    let (dir, repo) = fixture();
    fs::create_dir_all(dir.path().join("web/cache")).unwrap();
    fs::write(dir.path().join("web/.gitignore"), "cache/\n").unwrap();
    git(dir.path(), &["add", "web/.gitignore"]);
    git(dir.path(), &["commit", "-qm", "web"]);
    let (_watcher, count) = counting_watch(&repo);
    for i in 0..10 {
        fs::write(dir.path().join("web/cache/page.html"), format!("{i}")).unwrap();
    }
    sleep(SETTLE);
    assert_eq!(count.load(Ordering::SeqCst), 0, "a nested ignored directory was reported");
}

#[test]
fn an_idle_watched_repository_runs_almost_no_git() {
    let (_dir, repo) = fixture();
    let (_watcher, _count) = counting_watch(&repo);
    refresh(&repo);
    let before = repo.git_calls();
    for _ in 0..20 {
        refresh(&repo);
    }
    // Only resolving HEAD: no add, write-tree, diff-tree or ls-files.
    assert!(repo.git_calls() - before <= 20, "{} git processes for 20 idle refreshes", repo.git_calls() - before);
}

#[test]
fn concurrent_refreshes_share_one_snapshot() {
    let (_dir, repo) = fixture();
    let (_watcher, _count) = counting_watch(&repo);
    let expected = repo.snapshot().unwrap();
    let before = repo.git_calls();
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| assert_eq!(repo.snapshot().unwrap(), expected));
        }
    });
    assert_eq!(repo.git_calls(), before, "idle snapshots ran git");
}

/// Snapshots that re-add only the paths the watcher saw must equal full ones.
#[test]
fn incremental_snapshots_see_every_kind_of_edit() {
    let (dir, repo) = fixture();
    let (_watcher, _count) = counting_watch(&repo);
    let root = dir.path();
    let paths = |repo: &Repo| -> Vec<String> {
        let head = repo.head_tree().unwrap();
        repo.changes(&head).unwrap().files.into_iter().map(|c| c.path).collect()
    };

    fs::write(root.join("new.rb"), "x\n").unwrap();
    fs::create_dir_all(root.join("lib/deep")).unwrap();
    fs::write(root.join("lib/deep/a.rb"), "a\n").unwrap();
    sleep(SETTLE);
    assert_eq!(paths(&repo), ["app.rb", "lib/deep/a.rb", "new.rb"]);

    fs::remove_file(root.join("new.rb")).unwrap();
    // Created and gone before a snapshot: the pathspec fails, git adds all.
    fs::write(root.join("flash.rb"), "x\n").unwrap();
    fs::remove_file(root.join("flash.rb")).unwrap();
    fs::rename(root.join("lib/deep/a.rb"), root.join("lib/b.rb")).unwrap();
    sleep(SETTLE);
    assert_eq!(paths(&repo), ["app.rb", "lib/b.rb"]);

    // A full snapshot from scratch agrees.
    let fresh = Repo::discover(root).unwrap();
    let head = fresh.head_tree().unwrap();
    let full: Vec<String> = fresh.changes(&head).unwrap().files.into_iter().map(|c| c.path).collect();
    assert_eq!(full, paths(&repo));
}
