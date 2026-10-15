//! Drives `Remote` against the real agent through a stand-in for `ssh`
//! that runs the remote command locally with a temporary home directory.

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::Command,
    sync::{Mutex, mpsc},
    time::Duration,
};

use difftower_core::{Backend, Remote, SshTarget};

/// Tests point `HOME` and `DIFFTOWER_SSH` at their own directories.
static ENV: Mutex<()> = Mutex::new(());

/// Writes a stand-in for `ssh` that runs its last argument locally.
fn fake_ssh(dir: &Path, home: &Path) {
    let ssh = dir.join("ssh");
    fs::write(&ssh, "#!/bin/sh\nfor last; do :; done\nexec sh -c \"$last\"\n").unwrap();
    fs::set_permissions(&ssh, fs::Permissions::from_mode(0o755)).unwrap();
    // SAFETY: tests that touch the environment hold `ENV`.
    unsafe {
        std::env::set_var("DIFFTOWER_SSH", &ssh);
        std::env::set_var("HOME", home);
    }
}

/// The agent binary, leaked so it can live as long as a connection.
fn agent() -> &'static [u8] {
    fs::read(env!("CARGO_BIN_EXE_difftower-agent")).unwrap().leak()
}

fn git(dir: &Path, args: &[&str]) {
    assert!(Command::new("git").current_dir(dir).args(args).status().unwrap().success());
}

#[test]
fn installs_the_agent_and_serves_a_repository() {
    let _env = ENV.lock().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let repo = home.join("project");
    fs::create_dir_all(repo.join("tests")).unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "user.email", "t@t"]);
    git(&repo, &["config", "user.name", "t"]);
    fs::write(repo.join("lib.rs"), "fn a() {}\n").unwrap();
    fs::write(repo.join("tests/spec.rs"), "#[test] fn t() {}\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-qm", "init"]);

    // An agent left by an older Difftower version gets cleaned up.
    fs::create_dir_all(home.join(".cache/difftower")).unwrap();
    fs::write(home.join(".cache/difftower/agent-0.0.1-old"), "").unwrap();

    fake_ssh(tmp.path(), &home);

    let agent = agent();
    let target = SshTarget::parse("box:~/project").unwrap();
    let steps = std::cell::RefCell::new(Vec::new());
    let remote = Remote::connect(target.clone(), agent, &|s| steps.borrow_mut().push(s.to_owned())).unwrap();
    assert!(steps.borrow().iter().any(|s| s.contains("Installing")));
    let installed: Vec<_> = fs::read_dir(home.join(".cache/difftower")).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(installed.len(), 1, "old agents are removed: {installed:?}");

    assert_eq!(remote.name(), "project");
    assert_eq!(remote.host().as_deref(), Some("box"));
    assert!(remote.status().unwrap().changes.is_empty());

    let (tx, rx) = mpsc::channel();
    let _watch = remote.watch(Box::new(move || { let _ = tx.send(()); })).unwrap();
    fs::write(repo.join("tests/spec.rs"), "// gone\n").unwrap();
    rx.recv_timeout(Duration::from_secs(5)).expect("the agent reports changes");

    remote.save_rules(&["tests/**".to_owned()]).unwrap();
    remote.save_approvals("0000000000000001\ttests/spec.rs\n").unwrap();
    let status = remote.status().unwrap();
    let paths: Vec<_> = status.changes.iter().map(|c| c.path.as_str()).collect();
    assert_eq!(paths, [".difftower.toml", "tests/spec.rs"]);
    assert_eq!(status.rules, ["tests/**"]);
    assert_eq!(status.approvals, "0000000000000001\ttests/spec.rs\n");
    assert_eq!(remote.read_file("tests/spec.rs").unwrap(), b"// gone\n");
    let spec = status.changes.iter().find(|c| c.path == "tests/spec.rs").unwrap();
    assert!(remote.file_diff(spec).unwrap().contains("+// gone"));
    assert!(remote.read_file("../outside").is_err());

    let spec = status.changes.iter().find(|c| c.path == "tests/spec.rs").unwrap();
    remote.revert(spec).unwrap();
    assert_eq!(fs::read_to_string(repo.join("tests/spec.rs")).unwrap(), "#[test] fn t() {}\n");

    // A second connection reuses the installed agent.
    drop(remote);
    steps.borrow_mut().clear();
    Remote::connect(target, agent, &|s| steps.borrow_mut().push(s.to_owned())).unwrap();
    assert!(!steps.borrow().iter().any(|s| s.contains("Installing")));
}

#[test]
fn reports_a_path_that_is_not_a_repository() {
    let _env = ENV.lock().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    fake_ssh(tmp.path(), tmp.path());
    let agent = agent();
    let target = SshTarget::parse(&format!("box:{}", tmp.path().display())).unwrap();
    let err = Remote::connect(target, agent, &|_| {}).err().expect("connect fails");
    assert!(err.to_string().contains("not inside a git repository"), "{err}");
}

#[test]
fn reports_a_missing_directory() {
    let _env = ENV.lock().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    fake_ssh(tmp.path(), tmp.path());
    let agent = agent();
    let target = SshTarget::parse("box:/nonexistent/repo").unwrap();
    let err = Remote::connect(target, agent, &|_| {}).err().expect("connect fails");
    assert_eq!(err.to_string(), "/nonexistent/repo does not exist or is not a directory");
}

#[test]
fn serves_a_directory_of_repositories() {
    let _env = ENV.lock().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    for name in ["api", "web"] {
        let repo = home.join("repos").join(name);
        fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        fs::write(repo.join("README.md"), name).unwrap();
    }
    fake_ssh(tmp.path(), &home);
    let agent = agent();
    let remote = Remote::connect(SshTarget::parse("box:~/repos").unwrap(), agent, &|_| {}).unwrap();
    assert_eq!(remote.repo_count(), 2);
    assert_eq!(remote.name(), "repos");
    let paths: Vec<_> = remote.status().unwrap().changes.into_iter().map(|c| c.path).collect();
    assert_eq!(paths, ["api/README.md", "web/README.md"]);
}

/// The interface asks for a status after every change notification; if the
/// agent's own snapshots counted as changes, that would never stop.
#[test]
fn an_idle_repository_sends_no_change_notifications() {
    let _env = ENV.lock().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let repo = home.join("app");
    fs::create_dir_all(repo.join("log")).unwrap();
    git(&repo, &["init", "-q"]);
    fs::write(repo.join(".gitignore"), "/log/*\n").unwrap();
    fs::write(repo.join("app.rb"), "puts 1\n").unwrap();
    fake_ssh(tmp.path(), &home);
    let agent = agent();
    let remote = Remote::connect(SshTarget::parse("box:~/app").unwrap(), agent, &|_| {}).unwrap();

    let (tx, rx) = mpsc::channel();
    let _watch = remote.watch(Box::new(move || { let _ = tx.send(()); })).unwrap();
    std::thread::sleep(Duration::from_millis(600));
    while rx.try_recv().is_ok() {}
    for i in 0..10 {
        remote.status().unwrap();
        // A dev server writing its log on every request.
        fs::write(repo.join("log/development.log"), format!("GET / {i}\n")).unwrap();
    }
    assert!(
        rx.recv_timeout(Duration::from_millis(800)).is_err(),
        "refreshing an idle repository produced a change notification"
    );

    fs::write(repo.join("app.rb"), "puts 2\n").unwrap();
    rx.recv_timeout(Duration::from_secs(5)).expect("a real edit is still reported");
}

/// A dropped connection, here the agent dying on the host, heals on its own
/// and tells the interface to refresh.
#[test]
fn reconnects_after_the_connection_drops() {
    let _env = ENV.lock().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let repo = home.join("app");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    fs::write(repo.join("a.rb"), "1\n").unwrap();
    fake_ssh(tmp.path(), &home);
    let remote = Remote::connect(SshTarget::parse("box:~/app").unwrap(), agent(), &|_| {}).unwrap();
    let (tx, rx) = mpsc::channel();
    let _watch = remote.watch(Box::new(move || { let _ = tx.send(()); })).unwrap();
    assert_eq!(remote.status().unwrap().changes.len(), 1);

    let agents = home.join(".cache/difftower");
    assert!(Command::new("pkill").arg("-f").arg(&agents).status().unwrap().success());
    rx.recv_timeout(Duration::from_secs(5)).expect("the drop is reported");
    assert!(remote.status().unwrap_err().to_string().contains("reconnecting"));

    rx.recv_timeout(Duration::from_secs(10)).expect("the reconnection is reported");
    assert_eq!(remote.status().unwrap().changes.len(), 1);
    // The new agent watches the repository too.
    fs::write(repo.join("b.rb"), "2\n").unwrap();
    rx.recv_timeout(Duration::from_secs(5)).expect("changes are reported again");
    assert_eq!(remote.status().unwrap().changes.len(), 2);
}
