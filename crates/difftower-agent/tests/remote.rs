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

    let agent = fs::read(env!("CARGO_BIN_EXE_difftower-agent")).unwrap();
    let target = SshTarget::parse("box:~/project").unwrap();
    let steps = std::cell::RefCell::new(Vec::new());
    let remote = Remote::connect(target.clone(), &agent, &|s| steps.borrow_mut().push(s.to_owned())).unwrap();
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
    assert!(remote.read_file("../outside").is_err());

    let spec = status.changes.iter().find(|c| c.path == "tests/spec.rs").unwrap();
    remote.revert(&status.base, spec).unwrap();
    assert_eq!(fs::read_to_string(repo.join("tests/spec.rs")).unwrap(), "#[test] fn t() {}\n");

    // A second connection reuses the installed agent.
    drop(remote);
    steps.borrow_mut().clear();
    Remote::connect(target, &agent, &|s| steps.borrow_mut().push(s.to_owned())).unwrap();
    assert!(!steps.borrow().iter().any(|s| s.contains("Installing")));
}

#[test]
fn reports_a_path_that_is_not_a_repository() {
    let _env = ENV.lock().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    fake_ssh(tmp.path(), tmp.path());
    let agent = fs::read(env!("CARGO_BIN_EXE_difftower-agent")).unwrap();
    let target = SshTarget::parse(&format!("box:{}", tmp.path().display())).unwrap();
    let err = Remote::connect(target, &agent, &|_| {}).err().expect("connect fails");
    assert!(err.to_string().contains("not inside a git repository"), "{err}");
}

#[test]
fn reports_a_missing_directory() {
    let _env = ENV.lock().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    fake_ssh(tmp.path(), tmp.path());
    let agent = fs::read(env!("CARGO_BIN_EXE_difftower-agent")).unwrap();
    let target = SshTarget::parse("box:/nonexistent/repo").unwrap();
    let err = Remote::connect(target, &agent, &|_| {}).err().expect("connect fails");
    assert_eq!(err.to_string(), "/nonexistent/repo does not exist or is not a directory");
}
