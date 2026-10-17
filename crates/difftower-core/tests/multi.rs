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
    assert_eq!(backend.repos(), ["api", "web"]);

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
        backend.revert(std::slice::from_ref(change), false).unwrap();
    }
    assert!(backend.status().unwrap().changes.is_empty());
    assert!(root.path().join("api/old.rs").exists());
}

#[test]
fn keeps_rules_in_each_repository() {
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
}

#[test]
fn a_directory_without_repositories_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let err = open_path(dir.path()).err().unwrap();
    assert!(format!("{err:#}").contains("contains no repositories"), "{err:#}");
}

/// `root` is a repository too, holding `api`, which it ignores, and `web`,
/// which it neither ignores nor tracks.
fn nested_fixture() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    repo(root.path(), &[("AGENTS.md", "be nice\n"), ("library/book.md", "# book\n"), (".gitignore", "/api/\n")]);
    repo(&root.path().join("api"), &[("src/main.rs", "fn main() {}\n")]);
    repo(&root.path().join("web"), &[("index.html", "<p>hi</p>\n")]);
    root
}

#[test]
fn a_repository_holding_repositories_shows_them_all() {
    let root = nested_fixture();
    let backend = open_path(root.path()).unwrap();
    assert_eq!(backend.repos(), ["", "api", "web"]);

    fs::write(root.path().join("AGENTS.md"), "be kind\n").unwrap();
    fs::write(root.path().join("library/book.md"), "# a book\n").unwrap();
    fs::write(root.path().join("api/src/main.rs"), "fn main() { todo!() }\n").unwrap();
    fs::write(root.path().join("web/index.html"), "<p>bye</p>\n").unwrap();

    let status = backend.status().unwrap();
    let paths: Vec<_> = status.changes.iter().map(|c| c.path.as_str()).collect();
    assert_eq!(paths, ["AGENTS.md", "library/book.md", "api/src/main.rs", "web/index.html"]);
    assert!(status.changes[2].patch.contains("+++ b/api/src/main.rs"), "{}", status.changes[2].patch);

    for file in ["AGENTS.md", "library/book.md", "api/src/main.rs", "web/index.html"] {
        assert!(status.files.contains(&file.to_owned()), "{file} missing from {:?}", status.files);
    }
    // The inner repositories' own files, never the outer one's view of them.
    assert!(!status.files.iter().any(|f| f == "web" || f.starts_with("web/.git")), "{:?}", status.files);

    assert_eq!(backend.read_file("api/src/main.rs").unwrap(), b"fn main() { todo!() }\n");
    assert_eq!(backend.read_file("AGENTS.md").unwrap(), b"be kind\n");
    let full = backend.file_diff(&status.changes[0]).unwrap();
    assert!(full.starts_with("diff --git a/AGENTS.md b/AGENTS.md"), "{full}");
    let full = backend.file_diff(&status.changes[3]).unwrap();
    assert!(full.starts_with("diff --git a/web/index.html b/web/index.html"), "{full}");

    for change in &status.changes {
        backend.revert(std::slice::from_ref(change), false).unwrap();
    }
    assert!(backend.status().unwrap().changes.is_empty());
}

#[test]
fn a_repository_holding_repositories_keeps_rules_in_each() {
    let root = nested_fixture();
    let backend = open_path(root.path()).unwrap();

    backend.save_rules(&["library/**".into(), "api/src/**".into(), "!api/src/main.rs".into()]).unwrap();
    let outer = fs::read_to_string(root.path().join(".difftower.toml")).unwrap();
    assert!(outer.contains("\"library/**\"") && !outer.contains("api"), "{outer}");
    let api = fs::read_to_string(root.path().join("api/.difftower.toml")).unwrap();
    assert!(api.contains("\"src/**\"") && api.contains("\"!src/main.rs\""), "{api}");
    let mut rules = backend.status().unwrap().rules;
    rules.sort();
    assert_eq!(rules, ["!api/src/main.rs", "api/src/**", "library/**"]);
}

#[test]
fn a_repository_without_repositories_inside_stays_one() {
    let root = tempfile::tempdir().unwrap();
    repo(root.path(), &[("a.txt", "a\n")]);
    fs::create_dir(root.path().join("docs")).unwrap();
    assert_eq!(open_path(root.path()).unwrap().repos(), [""]);
    // Opening a subdirectory still opens the whole repository.
    assert_eq!(open_path(&root.path().join("docs")).unwrap().root(), root.path().canonicalize().unwrap().display().to_string());
}
