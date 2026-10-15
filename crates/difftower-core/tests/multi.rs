use std::{fs, path::Path, process::Command};

use difftower_core::open_path;

fn git(dir: &Path, args: &[&str]) {
    assert!(Command::new("git").current_dir(dir).args(args).status().unwrap().success());
}

fn repo(dir: &Path, files: &[(&str, &str)]) {
    fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q"]);
    git(dir, &["config", "user.email", "t@t"]);
    git(dir, &["config", "user.name", "t"]);
    for (path, content) in files {
        let path = dir.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }
    git(dir, &["add", "."]);
    git(dir, &["commit", "-qm", "init"]);
}

/// `root/{api,web}` are repositories; `root/notes` is a plain directory.
fn fixture() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    repo(
        &root.path().join("api"),
        &[("src/main.rs", "fn main() {}\n"), ("old.rs", "x\n"), (".difftower.toml", "[guard]\nprotect = [\"src/**\"]\n")],
    );
    repo(&root.path().join("web"), &[("index.html", "<p>hi</p>\n")]);
    fs::create_dir(root.path().join("notes")).unwrap();
    fs::write(root.path().join("notes/todo.md"), "- x\n").unwrap();
    root
}

#[test]
fn shows_every_repository_under_its_name() {
    let root = fixture();
    let backend = open_path(root.path()).unwrap();
    assert_eq!(backend.repo_count(), 2);

    fs::write(root.path().join("api/src/main.rs"), "fn main() { todo!() }\n").unwrap();
    fs::rename(root.path().join("api/old.rs"), root.path().join("api/new.rs")).unwrap();
    fs::write(root.path().join("web/index.html"), "<p>bye</p>\n").unwrap();
    fs::write(root.path().join("notes/todo.md"), "- y\n").unwrap();

    let status = backend.status().unwrap();
    let paths: Vec<_> = status.changes.iter().map(|c| c.path.as_str()).collect();
    assert_eq!(paths, ["api/new.rs", "api/src/main.rs", "web/index.html"]);
    let renamed = &status.changes[0];
    assert_eq!(renamed.old_path.as_deref(), Some("api/old.rs"));
    assert!(renamed.patch.contains("diff --git a/api/old.rs b/api/new.rs"), "{}", renamed.patch);
    assert!(renamed.patch.contains("rename from api/old.rs\nrename to api/new.rs"), "{}", renamed.patch);
    assert!(status.changes[1].patch.contains("+++ b/api/src/main.rs"));

    assert!(status.files.contains(&"web/index.html".to_owned()));
    assert!(!status.files.iter().any(|f| f.starts_with("notes")));
    assert_eq!(status.rules, ["api/src/**"]);

    assert_eq!(backend.read_file("web/index.html").unwrap(), b"<p>bye</p>\n");
    let full = backend.file_diff(&status.changes[0]).unwrap();
    assert!(full.contains("rename from api/old.rs\nrename to api/new.rs"), "{full}");
    let full = backend.file_diff(&status.changes[1]).unwrap();
    assert!(full.starts_with("diff --git a/api/src/main.rs b/api/src/main.rs"), "{full}");
    for change in &status.changes {
        backend.revert(change).unwrap();
    }
    assert!(backend.status().unwrap().changes.is_empty());
    assert!(root.path().join("api/old.rs").exists());
}

#[test]
fn keeps_rules_and_approvals_in_each_repository() {
    let root = fixture();
    let backend = open_path(root.path()).unwrap();

    backend.save_rules(&["api/src/**".into(), "web/**".into()]).unwrap();
    let web_config = fs::read_to_string(root.path().join("web/.difftower.toml")).unwrap();
    assert!(web_config.contains("\"**\""), "{web_config}");
    // api's rules did not change, so its file was not rewritten.
    assert_eq!(
        fs::read_to_string(root.path().join("api/.difftower.toml")).unwrap(),
        "[guard]\nprotect = [\"src/**\"]\n"
    );
    assert!(backend.save_rules(&["notes/**".into()]).is_err());

    // Exceptions keep their `!` in front of the repository's own path.
    backend.save_rules(&["api/src/**".into(), "!api/src/main.rs".into()]).unwrap();
    let api_config = fs::read_to_string(root.path().join("api/.difftower.toml")).unwrap();
    assert!(api_config.contains("\"!src/main.rs\""), "{api_config}");
    assert_eq!(backend.status().unwrap().rules, ["api/src/**", "!api/src/main.rs"]);

    backend.save_approvals("00000000000000aa\tweb/index.html\n").unwrap();
    assert_eq!(backend.status().unwrap().approvals, "00000000000000aa\tweb/index.html\n");
    assert!(!root.path().join("api/.git/difftower/approvals").exists());
}

#[test]
fn a_directory_without_repositories_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let err = open_path(dir.path()).err().unwrap();
    assert!(format!("{err:#}").contains("contains no repositories"), "{err:#}");
}
