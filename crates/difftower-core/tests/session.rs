//! A session compares against the working tree when the user started it, so
//! an agent committing does not hide what it changed, protected files
//! included. Without one, changes are against HEAD.

use std::{fs, path::Path, process::Command};

use difftower_core::open_path;

fn git(dir: &Path, args: &[&str]) {
    assert!(Command::new("git").current_dir(dir).args(args).status().unwrap().success());
}

/// A repository with one commit, and an edit the user had not committed
/// before the session.
fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git(root, &["init", "-q"]);
    git(root, &["config", "user.email", "t@t"]);
    git(root, &["config", "user.name", "t"]);
    fs::create_dir(root.join("tests")).unwrap();
    fs::write(root.join("app.rs"), "fn a() {}\n").unwrap();
    fs::write(root.join("tests/spec.rs"), "#[test] fn t() {}\n").unwrap();
    fs::write(root.join("notes.md"), "draft\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "init"]);
    fs::write(root.join("notes.md"), "draft, mine\n").unwrap();
    dir
}

fn paths(changes: &[difftower_core::Change]) -> Vec<&str> {
    changes.iter().map(|c| c.path.as_str()).collect()
}

/// What an agent does: edit, then commit.
fn agent_commits(root: &Path, file: &str, content: &str) {
    fs::write(root.join(file), content).unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", &format!("agent: {file}")]);
}

#[test]
fn without_a_session_changes_are_against_head() {
    let dir = fixture();
    let backend = open_path(dir.path()).unwrap();
    let status = backend.status().unwrap();
    assert_eq!(status.session, None);
    assert_eq!(paths(&status.changes), ["notes.md"]);
    assert!(backend.accept(&["notes.md".into()]).is_err(), "nothing to accept into");
}

#[test]
fn changes_stay_in_view_after_the_agent_commits_them() {
    let dir = fixture();
    let backend = open_path(dir.path()).unwrap();
    backend.start_session().unwrap();
    // What was uncommitted when the session started stays in view.
    let status = backend.status().unwrap();
    assert_eq!(paths(&status.changes), ["notes.md"]);
    assert!(status.session.unwrap().started > 0);

    agent_commits(dir.path(), "tests/spec.rs", "#[test] fn t() { assert!(false) }\n");
    agent_commits(dir.path(), "app.rs", "fn a() { b() }\n");

    let status = backend.status().unwrap();
    assert_eq!(paths(&status.changes), ["app.rs", "notes.md", "tests/spec.rs"]);
    assert!(status.changes[2].patch.contains("+#[test] fn t() { assert!(false) }"));
}

#[test]
fn the_session_outlives_a_restart() {
    let dir = fixture();
    open_path(dir.path()).unwrap().start_session().unwrap();
    agent_commits(dir.path(), "app.rs", "fn a() { b() }\n");

    let reopened = open_path(dir.path()).unwrap();
    assert_eq!(paths(&reopened.status().unwrap().changes), ["app.rs", "notes.md"]);
}

#[test]
fn accepting_a_file_takes_it_out_of_view_and_keeps_the_rest() {
    let dir = fixture();
    let backend = open_path(dir.path()).unwrap();
    backend.start_session().unwrap();
    let started = backend.status().unwrap().session.unwrap().started;
    agent_commits(dir.path(), "tests/spec.rs", "#[test] fn t() { assert!(false) }\n");
    fs::write(dir.path().join("app.rs"), "fn a() { b() }\n").unwrap();
    fs::write(dir.path().join("new.rs"), "fn n() {}\n").unwrap();
    fs::remove_file(dir.path().join("notes.md")).unwrap();

    backend.accept(&["tests/spec.rs".into(), "new.rs".into(), "notes.md".into()]).unwrap();
    let status = backend.status().unwrap();
    assert_eq!(paths(&status.changes), ["app.rs"]);
    assert_eq!(status.session.unwrap().started, started, "the session goes on");

    // A later edit to an accepted file shows again, against what was accepted.
    fs::write(dir.path().join("tests/spec.rs"), "// gone\n").unwrap();
    let status = backend.status().unwrap();
    assert_eq!(paths(&status.changes), ["app.rs", "tests/spec.rs"]);
    assert!(status.changes[1].patch.contains("-#[test] fn t() { assert!(false) }"));
}

#[test]
fn accepting_everything_ends_the_session() {
    let dir = fixture();
    let backend = open_path(dir.path()).unwrap();
    backend.start_session().unwrap();
    agent_commits(dir.path(), "app.rs", "fn a() { b() }\n");

    backend.end_session().unwrap();
    let status = backend.status().unwrap();
    assert_eq!(status.session, None);
    // Back to HEAD: the agent's commit is history, notes.md went with it.
    assert!(status.changes.is_empty());
}

#[test]
fn reverting_restores_the_session_start_and_can_be_undone() {
    let dir = fixture();
    let backend = open_path(dir.path()).unwrap();
    backend.start_session().unwrap();
    agent_commits(dir.path(), "notes.md", "overwritten by the agent\n");
    fs::write(dir.path().join("app.rs"), "fn a() { b() }\n").unwrap();
    fs::write(dir.path().join("new.rs"), "fn n() {}\n").unwrap();

    let changes = backend.status().unwrap().changes;
    assert_eq!(paths(&changes), ["app.rs", "new.rs", "notes.md"]);
    let picked: Vec<_> = changes.iter().filter(|c| c.path != "app.rs").cloned().collect();
    backend.revert(&picked, false).unwrap();
    // HEAD's when the session started, though the agent committed since;
    // its commit stays in history; the new file is gone.
    assert_eq!(fs::read_to_string(dir.path().join("notes.md")).unwrap(), "draft\n");
    assert!(!dir.path().join("new.rs").exists());
    let status = backend.status().unwrap();
    assert_eq!(paths(&status.changes), ["app.rs"]);
    assert!(status.can_undo);

    backend.undo().unwrap();
    assert_eq!(fs::read_to_string(dir.path().join("notes.md")).unwrap(), "overwritten by the agent\n");
    assert_eq!(fs::read_to_string(dir.path().join("new.rs")).unwrap(), "fn n() {}\n");
    let status = backend.status().unwrap();
    assert_eq!(paths(&status.changes), ["app.rs", "new.rs", "notes.md"]);
    assert!(!status.can_undo);
}

#[test]
fn reverting_everything_ends_the_session_and_undo_resumes_it() {
    let dir = fixture();
    let backend = open_path(dir.path()).unwrap();
    backend.start_session().unwrap();
    fs::write(dir.path().join("app.rs"), "fn a() { b() }\n").unwrap();

    let changes = backend.status().unwrap().changes;
    backend.revert(&changes, true).unwrap();
    let status = backend.status().unwrap();
    assert_eq!(status.session, None);
    assert_eq!(fs::read_to_string(dir.path().join("app.rs")).unwrap(), "fn a() {}\n");

    backend.undo().unwrap();
    let status = backend.status().unwrap();
    assert!(status.session.is_some());
    assert_eq!(paths(&status.changes), ["app.rs", "notes.md"]);
    assert_eq!(fs::read_to_string(dir.path().join("notes.md")).unwrap(), "draft, mine\n");
}

#[test]
fn sessions_work_across_a_directory_of_repositories() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["api", "web"] {
        let root = dir.path().join(name);
        fs::create_dir(&root).unwrap();
        git(&root, &["init", "-q"]);
        git(&root, &["config", "user.email", "t@t"]);
        git(&root, &["config", "user.name", "t"]);
        fs::write(root.join("lib.rs"), "fn a() {}\n").unwrap();
        git(&root, &["add", "."]);
        git(&root, &["commit", "-qm", "init"]);
    }
    let backend = open_path(dir.path()).unwrap();
    backend.start_session().unwrap();
    agent_commits(&dir.path().join("api"), "lib.rs", "fn a() { b() }\n");
    fs::write(dir.path().join("web/lib.rs"), "fn a() { c() }\n").unwrap();

    let status = backend.status().unwrap();
    assert!(status.session.is_some());
    assert_eq!(paths(&status.changes), ["api/lib.rs", "web/lib.rs"]);

    backend.accept(&["api/lib.rs".into()]).unwrap();
    let web: Vec<_> = backend.status().unwrap().changes;
    assert_eq!(paths(&web), ["web/lib.rs"]);
    backend.revert(&web, false).unwrap();
    assert!(backend.status().unwrap().changes.is_empty());
    backend.undo().unwrap();
    assert_eq!(paths(&backend.status().unwrap().changes), ["web/lib.rs"]);
}
