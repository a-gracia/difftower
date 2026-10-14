use std::{fs, path::Path, process::Command};

use difftower_core::{ChangeStatus, Guard, Repo};

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git").current_dir(dir).args(args).status().unwrap().success();
    assert!(ok, "git {args:?}");
}

fn fixture() -> (tempfile::TempDir, Repo) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git(root, &["init", "-q"]);
    git(root, &["config", "user.email", "t@t"]);
    git(root, &["config", "user.name", "t"]);
    fs::write(root.join(".gitignore"), "target/\n").unwrap();
    fs::write(root.join("a.txt"), "one\ntwo\n").unwrap();
    fs::write(root.join("b.txt"), "keep\n").unwrap();
    fs::create_dir(root.join("tests")).unwrap();
    fs::write(root.join("tests/spec.rs"), "assert!(true);\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "init"]);
    // An uncommitted edit before the session starts is part of the baseline.
    fs::write(root.join("b.txt"), "keep\nlocal\n").unwrap();
    let repo = Repo::discover(root).unwrap();
    (dir, repo)
}

#[test]
fn no_changes_right_after_baseline() {
    let (_dir, repo) = fixture();
    let base = repo.reset_baseline().unwrap();
    assert!(repo.changes(&base).unwrap().files.is_empty());
}

#[test]
fn reports_modified_added_deleted_and_skips_ignored() {
    let (dir, repo) = fixture();
    let root = dir.path();
    let base = repo.reset_baseline().unwrap();
    fs::write(root.join("a.txt"), "one\nTWO\n").unwrap();
    fs::write(root.join("new.txt"), "hello\n").unwrap();
    fs::remove_file(root.join("tests/spec.rs")).unwrap();
    fs::create_dir(root.join("target")).unwrap();
    fs::write(root.join("target/out"), "x").unwrap();

    let changes = repo.changes(&base).unwrap();
    let summary: Vec<_> = changes.files.iter().map(|c| (c.path.as_str(), c.status)).collect();
    assert_eq!(
        summary,
        [
            ("a.txt", ChangeStatus::Modified),
            ("new.txt", ChangeStatus::Added),
            ("tests/spec.rs", ChangeStatus::Deleted),
        ]
    );
    assert!(changes.files[0].patch.starts_with("diff --git a/a.txt b/a.txt"));
    assert!(changes.files[0].patch.contains("+TWO"));
    // The user's pre-session edit to b.txt is not reported.
    assert!(!changes.patch().contains("local"));
}

#[test]
fn commits_by_the_agent_do_not_hide_changes() {
    let (dir, repo) = fixture();
    let base = repo.reset_baseline().unwrap();
    fs::write(dir.path().join("a.txt"), "changed\n").unwrap();
    git(dir.path(), &["commit", "-qam", "agent"]);
    assert_eq!(repo.changes(&base).unwrap().files.len(), 1);
}

#[test]
fn revert_restores_each_kind_of_change() {
    let (dir, repo) = fixture();
    let root = dir.path();
    let base = repo.reset_baseline().unwrap();
    fs::write(root.join("a.txt"), "garbage\n").unwrap();
    fs::write(root.join("new.txt"), "hello\n").unwrap();
    fs::remove_file(root.join("tests/spec.rs")).unwrap();
    fs::rename(root.join("b.txt"), root.join("c.txt")).unwrap();

    for change in repo.changes(&base).unwrap().files {
        repo.revert(&base, &change).unwrap();
    }
    assert!(repo.changes(&base).unwrap().files.is_empty());
    assert_eq!(fs::read_to_string(root.join("b.txt")).unwrap(), "keep\nlocal\n");
}

#[test]
fn baseline_survives_reopening_until_cleared() {
    let (dir, repo) = fixture();
    let base = repo.reset_baseline().unwrap();
    let reopened = Repo::discover(dir.path().join("tests")).unwrap();
    assert_eq!(reopened.baseline().unwrap(), Some(base));
    reopened.clear_baseline().unwrap();
    assert_eq!(repo.baseline().unwrap(), None);
}

#[test]
fn diff_against_head_includes_uncommitted_and_untracked() {
    let (dir, repo) = fixture();
    fs::write(dir.path().join("new.txt"), "hello\n").unwrap();
    let head = repo.head_tree().unwrap();
    let changes = repo.changes(&head).unwrap();
    let paths: Vec<_> = changes.files.iter().map(|c| c.path.as_str()).collect();
    assert_eq!(paths, ["b.txt", "new.txt"]);

    // Once the agent commits, the diff against the new HEAD is clean.
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-qm", "agent"]);
    assert!(repo.changes(&repo.head_tree().unwrap()).unwrap().files.is_empty());
}

#[test]
fn head_of_a_repo_without_commits_is_the_empty_tree() {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    fs::write(dir.path().join("a.txt"), "a\n").unwrap();
    let repo = Repo::discover(dir.path()).unwrap();
    let changes = repo.changes(&repo.head_tree().unwrap()).unwrap();
    assert_eq!(changes.files[0].status, ChangeStatus::Added);
}

#[test]
fn guard_pins_and_globs() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join(".difftower.toml"), "[guard]\nprotect = [\"tests/**\"]\n").unwrap();
    let mut guard = Guard::load(dir.path()).unwrap();
    assert!(guard.is_protected("tests/spec.rs"));
    assert!(!guard.is_protected("src/main.rs"));

    guard.set_pinned("src/[weird].rs", true).unwrap();
    let guard = Guard::load(dir.path()).unwrap();
    assert!(guard.is_protected("src/[weird].rs"));
    assert!(!guard.is_protected("src/w.rs"));
    assert!(guard.is_protected("tests/x.rs"));
}
