use std::{fs, path::Path, process::Command};

use difftower_core::{Approvals, ChangeStatus, Guard, Repo, dir_rule, file_rule, load_rules, save_rules};

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
    // An uncommitted edit made before the snapshot.
    fs::write(root.join("b.txt"), "keep\nlocal\n").unwrap();
    let repo = Repo::discover(root).unwrap();
    (dir, repo)
}

#[test]
fn no_changes_against_a_fresh_snapshot() {
    let (_dir, repo) = fixture();
    let base = repo.snapshot().unwrap();
    assert!(repo.changes(&base).unwrap().files.is_empty());
}

#[test]
fn reports_modified_added_deleted_and_skips_ignored() {
    let (dir, repo) = fixture();
    let root = dir.path();
    let base = repo.snapshot().unwrap();
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
    // The edit to b.txt made before the snapshot is not reported.
    assert!(!changes.patch().contains("local"));
}

#[test]
fn commits_by_the_agent_do_not_hide_changes() {
    let (dir, repo) = fixture();
    let base = repo.snapshot().unwrap();
    fs::write(dir.path().join("a.txt"), "changed\n").unwrap();
    git(dir.path(), &["commit", "-qam", "agent"]);
    assert_eq!(repo.changes(&base).unwrap().files.len(), 1);
}

#[test]
fn revert_restores_each_kind_of_change() {
    let (dir, repo) = fixture();
    let root = dir.path();
    let base = repo.snapshot().unwrap();
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
fn guard_rules_for_files_and_dirs() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join(".difftower.toml"), "[guard]\nprotect = [\"tests/**\"]\n").unwrap();
    let guard = Guard::new(load_rules(dir.path()).unwrap()).unwrap();
    assert!(guard.is_protected("tests/spec.rs"));
    assert!(guard.is_protected("tests/deep/spec.rs"));
    assert!(!guard.is_protected("src/main.rs"));

    let guard = guard.with_rule(&file_rule("src/[weird].rs"), true).unwrap();
    let guard = guard.with_rule(&dir_rule("db"), true).unwrap();
    save_rules(dir.path(), guard.patterns()).unwrap();
    let guard = Guard::new(load_rules(dir.path()).unwrap()).unwrap();
    assert!(guard.is_protected("src/[weird].rs"));
    assert!(!guard.is_protected("src/w.rs"));
    assert!(guard.is_protected("db/migrations/001.sql"));
    assert!(guard.has_rule(&dir_rule("db")));
    assert_eq!(guard.rule_for("db/x.sql"), Some(2));
    assert!(!guard.with_rule("tests/**", false).unwrap().is_protected("tests/spec.rs"));
}

#[test]
fn exceptions_and_the_last_matching_rule_win() {
    let guard = Guard::new(vec!["tests/**".into(), "!tests/scratch/**".into(), "tests/scratch/keep.rs".into()]).unwrap();
    assert!(guard.is_protected("tests/a.rs"));
    assert!(!guard.is_protected("tests/scratch/tmp.rs"));
    assert!(guard.is_protected("tests/scratch/keep.rs"));
    assert_eq!(guard.rule_for("tests/a.rs"), Some(0));
    assert_eq!(guard.rule_for("tests/scratch/tmp.rs"), None);
}

#[test]
fn protecting_and_unprotecting_paths_keeps_rules_minimal() {
    let guard = Guard::empty().with_protected("tests", true, true).unwrap();
    assert_eq!(guard.patterns(), ["tests/**"]);

    // Unprotecting a file inside a protected folder adds an exception.
    let guard = guard.with_protected("tests/flaky.rs", false, false).unwrap();
    assert_eq!(guard.patterns(), ["tests/**", "!tests/flaky.rs"]);
    assert!(!guard.is_protected("tests/flaky.rs"));
    assert!(guard.is_protected("tests/ok.rs"));

    // Protecting it again just drops the exception.
    let back = guard.with_protected("tests/flaky.rs", false, true).unwrap();
    assert_eq!(back.patterns(), ["tests/**"]);

    // Protecting the folder again clears the rules inside it.
    let full = guard.with_protected("tests", true, true).unwrap();
    assert_eq!(full.patterns(), ["tests/**"]);

    // Unprotecting a folder inside a protected one adds a folder exception.
    let partial = full.with_protected("tests/fixtures", true, false).unwrap();
    assert_eq!(partial.patterns(), ["tests/**", "!tests/fixtures/**"]);
    assert!(!partial.is_protected("tests/fixtures/data.json"));

    // Unprotecting the outer folder removes everything about it.
    assert!(partial.with_protected("tests", true, false).unwrap().patterns().is_empty());
}

#[test]
fn lists_files_without_ignored_ones() {
    let (dir, repo) = fixture();
    fs::create_dir(dir.path().join("target")).unwrap();
    fs::write(dir.path().join("target/out"), "x").unwrap();
    fs::write(dir.path().join("new.txt"), "x").unwrap();
    repo.snapshot().unwrap();
    assert_eq!(repo.files().unwrap(), [".gitignore", "a.txt", "b.txt", "new.txt", "tests/spec.rs"]);
}

#[test]
fn approvals_cover_one_exact_patch_and_persist() {
    let (dir, repo) = fixture();
    let head = repo.head_tree().unwrap();
    fs::write(dir.path().join("tests/spec.rs"), "assert!(false);\n").unwrap();
    let change = repo.changes(&head).unwrap().files.into_iter().find(|c| c.path == "tests/spec.rs").unwrap();

    let mut approvals = Approvals::default();
    approvals.approve(&change);
    repo.write_approvals(&approvals.to_text()).unwrap();
    let mut approvals = Approvals::parse(&repo.read_approvals().unwrap());
    assert!(approvals.is_approved(&change));

    // A further edit by the agent is not covered by the earlier approval.
    fs::write(dir.path().join("tests/spec.rs"), "assert!(1 == 2);\n").unwrap();
    let changes = repo.changes(&head).unwrap().files;
    let edited = changes.iter().find(|c| c.path == "tests/spec.rs").unwrap();
    assert!(!approvals.is_approved(edited));
    assert!(approvals.retain_current(&changes));
    assert!(!approvals.is_approved(&change));
}
