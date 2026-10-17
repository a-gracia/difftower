//! Rust's own test harness, through `cargo test`.
//!
//! Listing reads the sources rather than asking the harness, which would
//! compile every test binary first: each crate target's root file is
//! followed through its `mod` declarations, and every function marked with
//! a `test` attribute (`#[test]`, `#[tokio::test]`, `#[rstest]`…) is named
//! by its module path, as the harness prints it. A test's id is its file
//! and that path: `crates/app/src/util.rs::util::tests::adds`.

use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

use anyhow::Result;

use super::{Blocks, Found, Kind, Outcome, Parser, Started, TestCase, TestTarget, quote, walk};

pub(crate) struct Cargo;

impl Kind for Cargo {
    fn detect(&self, root: &Path) -> bool {
        root.join("Cargo.toml").is_file()
    }

    fn default_command(&self, _root: &Path) -> String {
        "cargo test".to_owned()
    }

    fn discover(&self, root: &Path, _command: &str) -> Result<Vec<TestCase>> {
        Ok(scan(root).into_iter().map(|t| t.case).collect())
    }

    fn start(&self, root: &Path, command: &str, target: &TestTarget) -> Result<Started> {
        let tests = scan(root);
        let workspace = super::read(root, "Cargo.toml").lines().any(|l| l.trim() == "[workspace]");
        let script = script(command, workspace, &tests, target);
        Ok(Started { script, parser: Box::new(CargoParser::new(tests)) })
    }
}

/// A test the sources define.
#[derive(Clone, Debug)]
struct Test {
    case: TestCase,
    /// The package it belongs to.
    package: String,
    /// The target it is compiled into.
    target: Target,
    /// Its name in the harness: the module path.
    path: String,
}

/// A compiled test binary of a package.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Target {
    kind: TargetKind,
    /// As in the binary's file name in `target/debug/deps`: dashes become
    /// underscores.
    artifact: String,
    /// Its root file, relative to the package, as cargo prints it in
    /// `Running unittests src/lib.rs (…)`.
    root_file: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum TargetKind {
    Lib,
    Bin(String),
    Test(String),
}

impl Target {
    /// The `cargo test` options that build only this target.
    fn select(&self) -> String {
        match &self.kind {
            TargetKind::Lib => "--lib".to_owned(),
            TargetKind::Bin(name) => format!("--bin {}", quote(name)),
            TargetKind::Test(name) => format!("--test {}", quote(name)),
        }
    }
}

/// A package: its name, directory relative to the repository root (empty
/// for the root itself) and targets.
struct Package {
    name: String,
    dir: String,
    targets: Vec<Target>,
}

/// Every package under `root`, read from the manifests.
fn packages(root: &Path) -> Vec<Package> {
    let mut packages = Vec::new();
    for manifest in walk(root, |file| file == "Cargo.toml" || file.ends_with("/Cargo.toml")) {
        let Ok(value) = super::read(root, &manifest).parse::<toml::Table>() else { continue };
        let Some(name) = value.get("package").and_then(|p| p.get("name")).and_then(|n| n.as_str()) else {
            continue;
        };
        let dir = manifest.strip_suffix("Cargo.toml").unwrap_or("").trim_end_matches('/').to_owned();
        let pkg_dir = root.join(&dir);
        let artifact = |name: &str| name.replace('-', "_");
        let mut targets = Vec::new();
        let table = |key: &str| value.get(key).and_then(|v| v.as_table());
        let lib_path = table("lib").and_then(|l| l.get("path")).and_then(|p| p.as_str()).unwrap_or("src/lib.rs");
        if pkg_dir.join(lib_path).is_file() {
            let lib_name = table("lib").and_then(|l| l.get("name")).and_then(|n| n.as_str()).unwrap_or(name);
            targets.push(Target { kind: TargetKind::Lib, artifact: artifact(lib_name), root_file: lib_path.to_owned() });
        }
        // Targets declared in the manifest, then the ones cargo finds alone.
        let declared = |key: &str| -> Vec<(String, Option<String>)> {
            value
                .get(key)
                .and_then(|v| v.as_array())
                .into_iter()
                .flatten()
                .filter_map(|t| {
                    let name = t.get("name")?.as_str()?.to_owned();
                    Some((name, t.get("path").and_then(|p| p.as_str()).map(str::to_owned)))
                })
                .collect()
        };
        let mut bins: Vec<(String, String)> = declared("bin")
            .into_iter()
            .map(|(n, p)| {
                let p = p.unwrap_or_else(|| {
                    if pkg_dir.join(format!("src/bin/{n}.rs")).is_file() { format!("src/bin/{n}.rs") } else { "src/main.rs".to_owned() }
                });
                (n, p)
            })
            .collect();
        let mut tests: Vec<(String, String)> =
            declared("test").into_iter().map(|(n, p)| { let p = p.unwrap_or_else(|| format!("tests/{n}.rs")); (n, p) }).collect();
        if pkg_dir.join("src/main.rs").is_file() && !bins.iter().any(|(_, p)| p == "src/main.rs") {
            bins.push((name.to_owned(), "src/main.rs".to_owned()));
        }
        for (dir_name, into) in [("src/bin", &mut bins), ("tests", &mut tests)] {
            let Ok(entries) = std::fs::read_dir(pkg_dir.join(dir_name)) else { continue };
            let mut found: Vec<(String, String)> = entries
                .flatten()
                .filter_map(|e| {
                    let file = e.file_name().to_string_lossy().into_owned();
                    let path = e.path();
                    if path.is_file() {
                        let stem = file.strip_suffix(".rs")?;
                        Some((stem.to_owned(), format!("{dir_name}/{file}")))
                    } else {
                        path.join("main.rs").is_file().then(|| (file.clone(), format!("{dir_name}/{file}/main.rs")))
                    }
                })
                .collect();
            found.sort();
            for (n, p) in found {
                if !into.iter().any(|(_, existing)| *existing == p) {
                    into.push((n, p));
                }
            }
        }
        targets.extend(bins.into_iter().map(|(n, p)| Target { artifact: artifact(&n), kind: TargetKind::Bin(n), root_file: p }));
        targets.extend(tests.into_iter().map(|(n, p)| Target { artifact: artifact(&n), kind: TargetKind::Test(n), root_file: p }));
        packages.push(Package { name: name.to_owned(), dir, targets });
    }
    packages
}

/// Every test of every package under `root`.
fn scan(root: &Path) -> Vec<Test> {
    let mut tests = Vec::new();
    for package in packages(root) {
        let join = |file: &str| if package.dir.is_empty() { file.to_owned() } else { format!("{}/{file}", package.dir) };
        for target in &package.targets {
            let root_file = join(&target.root_file);
            let child_dir = parent(&root_file).to_owned();
            let mut seen = HashSet::new();
            walk_module(root, &root_file, &[], &child_dir, &mut seen, &mut |file, path, line| {
                let path = path.join("::");
                let name = path.clone();
                tests.push(Test {
                    case: TestCase { id: format!("{file}::{path}"), file: file.to_owned(), name, line: Some(line) },
                    package: package.name.clone(),
                    target: target.clone(),
                    path,
                });
            });
        }
    }
    tests
}

fn parent(file: &str) -> &str {
    file.rsplit_once('/').map_or("", |(dir, _)| dir)
}

fn join_dir(dir: &str, rest: &str) -> String {
    if dir.is_empty() { rest.to_owned() } else { format!("{dir}/{rest}") }
}

/// Reads `file`, a module at `module`, whose `mod x;` children live under
/// `child_dir`, calling `on_test` with each test's file, path and line.
fn walk_module(
    root: &Path,
    file: &str,
    module: &[String],
    child_dir: &str,
    seen: &mut HashSet<String>,
    on_test: &mut dyn FnMut(&str, &[String], u32),
) {
    if !seen.insert(file.to_owned()) {
        return;
    }
    let Ok(text) = std::fs::read_to_string(root.join(file)) else { return };
    for item in items(&text) {
        let mut path = module.to_vec();
        path.extend(item.inline.iter().cloned());
        match item.kind {
            ItemKind::Test(name) => {
                path.push(name);
                on_test(file, &path, item.line);
            }
            ItemKind::Mod { name, path: attr } => {
                let inner_dir = item.inline.iter().fold(child_dir.to_owned(), |dir, m| join_dir(&dir, m));
                let (child, grand) = match attr {
                    // `#[path]` is relative to the declaring file's directory.
                    Some(p) => {
                        let f = join_dir(parent(file), &p);
                        let g = join_dir(parent(&f), &name);
                        (f, g)
                    }
                    None => {
                        let flat = join_dir(&inner_dir, &format!("{name}.rs"));
                        let nested = join_dir(&inner_dir, &format!("{name}/mod.rs"));
                        let f = if root.join(&flat).is_file() { flat } else { nested };
                        (f, join_dir(&inner_dir, &name))
                    }
                };
                path.push(name);
                walk_module(root, &child, &path, &grand, seen, on_test);
            }
        }
    }
}

struct Item {
    /// The inline modules (`mod x { … }`) it sits in, outermost first.
    inline: Vec<String>,
    kind: ItemKind,
    line: u32,
}

enum ItemKind {
    /// A function marked as a test.
    Test(String),
    /// `mod name;`, its file elsewhere, maybe given by `#[path = "…"]`.
    Mod { name: String, path: Option<String> },
}

/// The tests and out-of-line modules of a Rust source file. A light
/// reading, not a parser: comments and literals are blanked out, braces
/// counted to know which inline module each item is in.
fn items(text: &str) -> Vec<Item> {
    let code = blank_literals(text);
    let mut items = Vec::new();
    let mut depth = 0usize;
    // Inline modules, with the depth of their bodies.
    let mut modules: Vec<(String, usize)> = Vec::new();
    let mut pending_test = false;
    let mut pending_path: Option<String> = None;
    for (ix, (line, raw)) in code.lines().zip(text.lines()).enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("#[") || trimmed.starts_with("#![") {
            let attr = trimmed.trim_start_matches("#[").trim_start_matches("#![");
            let head: String = attr.chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == ':').collect();
            if head.rsplit("::").next() == Some("test") || head == "rstest" {
                pending_test = true;
            }
            if head == "path" {
                pending_path = raw.split('"').nth(1).map(str::to_owned);
            }
        }
        let words: Vec<&str> = trimmed
            .split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .filter(|w| !w.is_empty())
            .collect();
        // Past visibility and qualifiers: `pub(crate) async unsafe fn x`.
        let skip = ["pub", "crate", "super", "self", "in", "async", "unsafe", "const", "extern"];
        let mut rest = words.iter().skip_while(|w| skip.contains(w));
        let inline: Vec<String> = modules.iter().map(|(m, _)| m.clone()).collect();
        match (rest.next(), rest.next()) {
            (Some(&"fn"), Some(name)) if !trimmed.starts_with("#") => {
                if std::mem::take(&mut pending_test) {
                    items.push(Item { inline, kind: ItemKind::Test((*name).to_owned()), line: ix as u32 + 1 });
                }
                pending_path = None;
            }
            (Some(&"mod"), Some(name)) if !trimmed.starts_with("#") => {
                let after = trimmed.split_once(name).map_or("", |(_, a)| a).trim_start();
                if after.starts_with(';') {
                    items.push(Item {
                        inline,
                        kind: ItemKind::Mod { name: (*name).to_owned(), path: pending_path.take() },
                        line: ix as u32 + 1,
                    });
                } else {
                    modules.push(((*name).to_owned(), depth + 1));
                }
                pending_test = false;
                pending_path = None;
            }
            _ => {}
        }
        for c in line.chars() {
            match c {
                '{' => depth += 1,
                '}' => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
        while modules.last().is_some_and(|(_, body)| *body > depth) {
            modules.pop();
        }
    }
    items
}

/// `text` with comments, strings and character literals turned into
/// spaces, lines kept.
fn blank_literals(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let blank = |out: &mut String, c: char| out.push(if c == '\n' { '\n' } else { ' ' });
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if c == '/' && next == Some('/') {
            while i < chars.len() && chars[i] != '\n' {
                blank(&mut out, chars[i]);
                i += 1;
            }
        } else if c == '/' && next == Some('*') {
            let mut level = 0;
            while i < chars.len() {
                if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
                    level += 1;
                    out.push_str("  ");
                    i += 2;
                } else if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                    level -= 1;
                    out.push_str("  ");
                    i += 2;
                    if level == 0 {
                        break;
                    }
                } else {
                    blank(&mut out, chars[i]);
                    i += 1;
                }
            }
        } else if c == 'r' && (next == Some('"') || next == Some('#')) && !prev_is_ident(&chars, i) {
            // A raw string: r"…" or r#"…"#.
            let mut j = i + 1;
            let mut hashes = 0;
            while chars.get(j) == Some(&'#') {
                hashes += 1;
                j += 1;
            }
            if chars.get(j) != Some(&'"') {
                out.push(c);
                i += 1;
                continue;
            }
            j += 1;
            loop {
                match chars.get(j) {
                    None => break,
                    Some('"') if (1..=hashes).all(|k| chars.get(j + k) == Some(&'#')) => {
                        j += 1 + hashes;
                        break;
                    }
                    _ => j += 1,
                }
            }
            for &c in &chars[i..j.min(chars.len())] {
                blank(&mut out, c);
            }
            i = j;
        } else if c == '"' {
            blank(&mut out, c);
            i += 1;
            while i < chars.len() && chars[i] != '"' {
                if chars[i] == '\\' {
                    blank(&mut out, chars[i]);
                    i += 1;
                }
                if i < chars.len() {
                    blank(&mut out, chars[i]);
                    i += 1;
                }
            }
            if i < chars.len() {
                blank(&mut out, chars[i]);
                i += 1;
            }
        } else if c == '\'' {
            // A character literal, '{' or '\n' or '\u{7b}', not a lifetime.
            let end = if next == Some('\\') {
                (i + 2..chars.len().min(i + 12)).find(|&j| chars[j] == '\'')
            } else {
                (chars.get(i + 2) == Some(&'\'')).then_some(i + 2)
            };
            match end {
                Some(end) => {
                    for &c in &chars[i..=end] {
                        blank(&mut out, c);
                    }
                    i = end + 1;
                }
                None => {
                    out.push(c);
                    i += 1;
                }
            }
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

fn prev_is_ident(chars: &[char], i: usize) -> bool {
    i > 0 && (chars[i - 1].is_alphanumeric() || chars[i - 1] == '_')
}

/// A package and maybe one of its targets, with the exact test names to
/// run there; no names runs them all.
type Group = ((String, Option<Target>), Option<Vec<String>>);

/// The script that runs `target`: one `cargo test` per package and test
/// binary involved, so that each gets its own name filters.
fn script(command: &str, workspace: bool, tests: &[Test], target: &TestTarget) -> String {
    const OPTS: &str = "--no-fail-fast";
    let all = || format!("{command} {}{OPTS}", if workspace { "--workspace " } else { "" });
    let mut groups: Vec<Group> = Vec::new();
    let mut add = |package: &str, target: Option<&Target>, name: Option<&str>| {
        let key = (package.to_owned(), target.cloned());
        let at = match groups.iter().position(|(k, _)| *k == key) {
            Some(at) => at,
            None => {
                groups.push((key, Some(Vec::new())));
                groups.len() - 1
            }
        };
        match (&mut groups[at].1, name) {
            (Some(names), Some(name)) => {
                if !names.iter().any(|n| n == name) {
                    names.push(name.to_owned());
                }
            }
            (filter, None) => *filter = None,
            (None, Some(_)) => {}
        }
    };
    match target {
        TestTarget::All => return all(),
        TestTarget::Paths { paths } => {
            let mut whole: Vec<String> = Vec::new();
            for path in paths {
                let path = path.trim_end_matches('/');
                let under = |file: &str| path.is_empty() || file == path || file.starts_with(&format!("{path}/"));
                let matching: Vec<&Test> = tests.iter().filter(|t| under(&t.case.file)).collect();
                if path.is_empty() || path == "." {
                    return all();
                }
                // A package's directory, or a directory around packages: run
                // each package whole.
                let packages: HashSet<&str> = matching.iter().map(|t| t.package.as_str()).collect();
                let covers = |package: &str| tests.iter().filter(|t| t.package == package).all(|t| under(&t.case.file));
                for test in &matching {
                    if covers(&test.package) {
                        if !whole.contains(&test.package) {
                            whole.push(test.package.clone());
                        }
                    } else if packages.len() == 1 && test.target.kind != TargetKind::Lib && file_is_root(test) {
                        add(&test.package, Some(&test.target), None);
                    } else {
                        add(&test.package, Some(&test.target), Some(&test.path));
                    }
                }
            }
            for package in whole {
                groups.retain(|((p, _), _)| *p != package);
                groups.push(((package, None), None));
            }
        }
        TestTarget::Cases { cases } => {
            let by_id: HashMap<&str, &Test> = tests.iter().map(|t| (t.case.id.as_str(), t)).collect();
            for case in cases {
                match by_id.get(case.id.as_str()) {
                    Some(test) => add(&test.package, Some(&test.target), Some(&test.path)),
                    // Not in the sources as read: run it by name everywhere.
                    None => {
                        let name = case.id.split_once("::").map_or(case.id.as_str(), |(_, n)| n);
                        return format!("{} -- {}", all(), quote(name));
                    }
                }
            }
        }
    }
    if groups.is_empty() {
        return all();
    }
    let commands: Vec<String> = groups
        .into_iter()
        .map(|((package, target), names)| {
            let mut cmd = format!("{command} -p {}", quote(&package));
            if let Some(target) = target {
                cmd.push(' ');
                cmd.push_str(&target.select());
            }
            cmd.push_str(&format!(" {OPTS}"));
            if let Some(names) = names {
                let names: Vec<String> = names.iter().map(|n| quote(n)).collect();
                cmd.push_str(&format!(" -- --exact {}", names.join(" ")));
            }
            cmd
        })
        .collect();
    if commands.len() == 1 {
        return commands.into_iter().next().unwrap();
    }
    // Every command runs; the run fails if any does.
    let mut script = "s=0".to_owned();
    for cmd in commands {
        script.push_str(&format!("; {cmd} || s=1"));
    }
    script.push_str("; exit $s");
    script
}

/// Whether the test is in its target's root file, so a path naming that
/// file means the whole target (an integration test file, say).
fn file_is_root(test: &Test) -> bool {
    test.case.file.ends_with(&test.target.root_file)
}

/// Reads the harness's output: `Running` lines say which binary runs,
/// `test path ... ok` lines give results, `---- path stdout ----` blocks
/// what failed.
struct CargoParser {
    /// (artifact, root file, path) to test ids.
    ids: HashMap<(String, String, String), String>,
    /// The binary running: its artifact and root file.
    current: Option<(String, String)>,
    blocks: Blocks,
}

impl CargoParser {
    fn new(tests: Vec<Test>) -> Self {
        let ids = tests
            .into_iter()
            .map(|t| ((t.target.artifact, t.target.root_file, t.path), t.case.id))
            .collect();
        Self { ids, current: None, blocks: Blocks::default() }
    }

    fn id(&self, path: &str) -> Option<String> {
        let (artifact, root_file) = self.current.as_ref()?;
        self.ids.get(&(artifact.clone(), root_file.clone(), path.to_owned())).cloned()
    }
}

impl Parser for CargoParser {
    fn feed(&mut self, line: &str) -> Vec<Found> {
        let trimmed = line.trim();
        // `Running unittests src/lib.rs (target/debug/deps/app-0123abcd)`
        // or `Running tests/api.rs (…)`.
        if let Some(rest) = trimmed.strip_prefix("Running ") {
            let found = self.blocks.end();
            let rest = rest.strip_prefix("unittests ").unwrap_or(rest);
            if let Some((file, binary)) = rest.split_once(" (") {
                let binary = binary.trim_end_matches(')');
                let name = binary.rsplit(['/', '\\']).next().unwrap_or(binary);
                let artifact = name.rsplit_once('-').map_or(name, |(a, _)| a);
                self.current = Some((artifact.to_owned(), file.to_owned()));
            }
            return found;
        }
        if trimmed.starts_with("Doc-tests ") {
            self.current = None;
            return self.blocks.end();
        }
        if let Some(rest) = line.strip_prefix("test ")
            && let Some((path, result)) = rest.rsplit_once(" ... ")
        {
            let outcome = match result.split(',').next().unwrap_or(result).trim() {
                "ok" => Outcome::Passed,
                "FAILED" => Outcome::Failed,
                "ignored" => Outcome::Skipped,
                _ => return Vec::new(),
            };
            return self.id(path).map(|id| Found::Result(id, outcome)).into_iter().collect();
        }
        if let Some(path) = line
            .strip_prefix("---- ")
            .and_then(|r| r.strip_suffix(" stdout ----").or_else(|| r.strip_suffix(" stderr ----")))
        {
            let ids = self.id(path).into_iter().collect();
            return self.blocks.start(ids);
        }
        // The list of failures after the blocks, or the summary.
        if line == "failures:" || line.starts_with("test result: ") {
            return self.blocks.end();
        }
        if self.blocks.reading() && (self.blocks.has_lines() || !line.trim().is_empty()) {
            self.blocks.push(line);
        }
        Vec::new()
    }

    fn finish(&mut self) -> Vec<Found> {
        self.blocks.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{
        CaseRef,
        test_support::{failures, parse, results},
    };

    /// A workspace of two packages, as the real one in the tests below was.
    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let files: &[(&str, &str)] = &[
            ("Cargo.toml", "[workspace]\nmembers = [\"crates/alpha\", \"crates/beta\"]\n"),
            ("crates/alpha/Cargo.toml", "[package]\nname = \"alpha-lib\"\nversion = \"0.1.0\"\n"),
            (
                "crates/alpha/src/lib.rs",
                "pub mod util;\n\n#[cfg(test)]\nmod tests {\n    use super::*;\n    #[test]\n    fn adds() {}\n    #[test]\n    fn fails() { assert_eq!(1, 2, \"math is hard {}\", '}'); }\n    #[test]\n    #[ignore]\n    fn later() {}\n}\n",
            ),
            ("crates/alpha/src/util/mod.rs", "pub mod deep;\n/// fn not_a_test() {}\n#[test]\nfn top_level_in_util() {}\n"),
            (
                "crates/alpha/src/util/deep.rs",
                "#[cfg(test)]\nmod tests {\n    #[tokio::test(flavor = \"multi_thread\")]\n    async fn deep_one() {}\n    mod inner {\n        #[test]\n        fn nested() { let s = r#\"}\"#; }\n    }\n}\nfn helper() {}\n",
            ),
            ("crates/alpha/tests/integration.rs", "#[test]\nfn it_works() {}\n#[test]\nfn it_fails() {}\n"),
            ("crates/beta/Cargo.toml", "[package]\nname = \"beta\"\nversion = \"0.1.0\"\n"),
            ("crates/beta/src/main.rs", "fn main() {}\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn adds() {}\n}\n"),
        ];
        for (path, text) in files {
            let path = dir.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        dir
    }

    #[test]
    fn lists_tests_by_module_path() {
        let dir = fixture();
        let mut ids: Vec<String> = Cargo.discover(dir.path(), "cargo test").unwrap().into_iter().map(|t| t.id).collect();
        ids.sort();
        assert_eq!(
            ids,
            [
                "crates/alpha/src/lib.rs::tests::adds",
                "crates/alpha/src/lib.rs::tests::fails",
                "crates/alpha/src/lib.rs::tests::later",
                "crates/alpha/src/util/deep.rs::util::deep::tests::deep_one",
                "crates/alpha/src/util/deep.rs::util::deep::tests::inner::nested",
                "crates/alpha/src/util/mod.rs::util::top_level_in_util",
                "crates/alpha/tests/integration.rs::it_fails",
                "crates/alpha/tests/integration.rs::it_works",
                "crates/beta/src/main.rs::tests::adds",
            ]
        );
        let tests = Cargo.discover(dir.path(), "cargo test").unwrap();
        let nested = tests.iter().find(|t| t.name == "util::deep::tests::inner::nested").unwrap();
        assert_eq!(nested.line, Some(7));
    }

    #[test]
    fn runs_what_is_asked_and_only_that() {
        let dir = fixture();
        let tests = scan(dir.path());
        let cases = |ids: &[&str]| TestTarget::Cases {
            cases: ids.iter().map(|id| CaseRef { id: id.to_string(), line: None }).collect(),
        };
        assert_eq!(script("cargo test", true, &tests, &TestTarget::All), "cargo test --workspace --no-fail-fast");
        assert_eq!(
            script("cargo test", true, &tests, &cases(&["crates/alpha/src/lib.rs::tests::adds"])),
            "cargo test -p 'alpha-lib' --lib --no-fail-fast -- --exact 'tests::adds'"
        );
        assert_eq!(
            script(
                "cargo test",
                true,
                &tests,
                &cases(&["crates/alpha/src/lib.rs::tests::adds", "crates/alpha/tests/integration.rs::it_fails"])
            ),
            "s=0; cargo test -p 'alpha-lib' --lib --no-fail-fast -- --exact 'tests::adds' || s=1; \
             cargo test -p 'alpha-lib' --test 'integration' --no-fail-fast -- --exact 'it_fails' || s=1; exit $s"
        );
        let paths = |paths: &[&str]| TestTarget::Paths { paths: paths.iter().map(|p| p.to_string()).collect() };
        assert_eq!(
            script("cargo test", true, &tests, &paths(&["crates/alpha/tests/integration.rs"])),
            "cargo test -p 'alpha-lib' --test 'integration' --no-fail-fast"
        );
        assert_eq!(
            script("cargo test", true, &tests, &paths(&["crates/alpha/src/util"])),
            "cargo test -p 'alpha-lib' --lib --no-fail-fast -- --exact 'util::deep::tests::deep_one' \
             'util::deep::tests::inner::nested' 'util::top_level_in_util'"
        );
        assert_eq!(script("cargo test", true, &tests, &paths(&["crates/beta"])), "cargo test -p 'beta' --no-fail-fast");
        assert_eq!(
            script("cargo test", true, &tests, &paths(&["crates"])),
            "s=0; cargo test -p 'alpha-lib' --no-fail-fast || s=1; cargo test -p 'beta' --no-fail-fast || s=1; exit $s"
        );
    }

    /// What `cargo test --no-fail-fast` printed for the fixture, cut short.
    const OUTPUT: &str = "\
   Compiling alpha-lib v0.1.0 (/tmp/x/crates/alpha)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.03s
     Running unittests src/lib.rs (target/debug/deps/alpha_lib-06bfbddcfaf54204)

running 6 tests
test tests::later ... ignored
test tests::adds ... ok
test util::deep::tests::deep_one ... ok
test tests::fails ... FAILED
test util::top_level_in_util ... ok
test util::deep::tests::inner::nested ... FAILED

failures:

---- tests::fails stdout ----

thread 'tests::fails' (1828456) panicked at crates/alpha/src/lib.rs:10:18:
assertion `left == right` failed: math is hard
  left: 3
 right: 4
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace

---- util::deep::tests::inner::nested stdout ----

thread 'util::deep::tests::inner::nested' (1828458) panicked at crates/alpha/src/util/deep.rs:7:23:
boom


failures:
    tests::fails
    util::deep::tests::inner::nested

test result: FAILED. 3 passed; 2 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.00s

error: test failed, to rerun pass `-p alpha-lib --lib`
     Running tests/integration.rs (target/debug/deps/integration-c526a767218935a8)

running 2 tests
test it_works ... ok
test it_fails ... FAILED

failures:

---- it_fails stdout ----

thread 'it_fails' (1828461) panicked at crates/alpha/tests/integration.rs:4:17:
assertion failed: 1 + 1 == 3
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace


failures:
    it_fails

test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

error: test failed, to rerun pass `-p alpha-lib --test integration`
     Running unittests src/main.rs (target/debug/deps/beta-ea8f310d7024e68b)

running 1 test
test tests::adds ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests alpha_lib

running 0 tests
";

    #[test]
    fn reads_results_and_failures_by_binary() {
        let dir = fixture();
        let found = parse(Box::new(CargoParser::new(scan(dir.path()))), OUTPUT);
        assert_eq!(
            results(&found),
            [
                ("crates/alpha/src/lib.rs::tests::later", Outcome::Skipped),
                ("crates/alpha/src/lib.rs::tests::adds", Outcome::Passed),
                ("crates/alpha/src/util/deep.rs::util::deep::tests::deep_one", Outcome::Passed),
                ("crates/alpha/src/lib.rs::tests::fails", Outcome::Failed),
                ("crates/alpha/src/util/mod.rs::util::top_level_in_util", Outcome::Passed),
                ("crates/alpha/src/util/deep.rs::util::deep::tests::inner::nested", Outcome::Failed),
                ("crates/alpha/tests/integration.rs::it_works", Outcome::Passed),
                ("crates/alpha/tests/integration.rs::it_fails", Outcome::Failed),
                // Same path as the library's, another binary.
                ("crates/beta/src/main.rs::tests::adds", Outcome::Passed),
            ]
        );
        let failed = failures(&found);
        assert_eq!(failed.len(), 3);
        assert_eq!(failed[0].0, "crates/alpha/src/lib.rs::tests::fails");
        assert_eq!(failed[0].1[0], "thread 'tests::fails' (1828456) panicked at crates/alpha/src/lib.rs:10:18:");
        assert_eq!(failed[0].1.last().unwrap(), "note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace");
        assert_eq!(failed[1].1.last().unwrap(), "boom");
        assert_eq!(failed[2].0, "crates/alpha/tests/integration.rs::it_fails");
    }
}
