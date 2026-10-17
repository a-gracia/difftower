//! Mocha, read through its `json-stream` reporter: a JSON line per test,
//! as it finishes.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use anyhow::Result;
use serde::Deserialize;

use super::jest::{is_script, js_tests, parse_js, relative, roots, spaced};
use super::{
    Found, Kind, Outcome, Parser, Started, TestCase, TestTarget, file_of, quote, read,
    regex_escape, shell_run, target_args, walk,
};

pub(crate) struct Mocha;

impl Kind for Mocha {
    fn detect(&self, root: &Path) -> bool {
        let config = [
            ".mocharc.js",
            ".mocharc.cjs",
            ".mocharc.json",
            ".mocharc.jsonc",
            ".mocharc.yml",
            ".mocharc.yaml",
        ]
        .iter()
        .any(|name| root.join(name).is_file());
        config || read(root, "package.json").contains("\"mocha\"")
    }

    fn default_command(&self, root: &Path) -> String {
        if root.join("node_modules/.bin/mocha").is_file() {
            "node_modules/.bin/mocha".to_owned()
        } else {
            "npx mocha".to_owned()
        }
    }

    fn discover(&self, root: &Path, command: &str) -> Result<Vec<TestCase>> {
        // A dry run names the files the project's configuration picks.
        let (out, _) = shell_run(root, &format!("{command} --dry-run --reporter json-stream"))?;
        let roots = roots(root);
        let mut files: Vec<String> = out
            .lines()
            .filter_map(|line| relative(&roots, &event(line)?.1.file?))
            .collect();
        files.sort();
        files.dedup();
        if files.is_empty() {
            // Mocha's default: the scripts in `test/`.
            files = walk(root, |f| f.starts_with("test/") && is_script(f));
        }
        Ok(js_tests(root, &files))
    }

    fn start(&self, root: &Path, command: &str, target: &TestTarget) -> Result<Started> {
        let args = match target {
            TestTarget::Cases { cases } => {
                let mut files: Vec<String> =
                    cases.iter().map(|c| file_of(&c.id).to_owned()).collect();
                files.dedup();
                let names: Vec<String> =
                    cases.iter().map(|c| regex_escape(&spaced(&c.id))).collect();
                let files = files.iter().map(|f| quote(f)).collect::<Vec<_>>().join(" ");
                format!(
                    "{files} --grep {}",
                    quote(&format!("^({})$", names.join("|")))
                )
            }
            TestTarget::Paths { .. } => {
                format!("--recursive {}", target_args(target, |c| c.id.clone()))
            }
            TestTarget::All => String::new(),
        };
        Ok(Started {
            script: format!("{command} --reporter json-stream {args}"),
            parser: Box::new(MochaParser {
                root: root.to_path_buf(),
                roots: roots(root),
                names: HashMap::new(),
            }),
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Event {
    full_title: Option<String>,
    passes: Option<u32>,
    failures: Option<u32>,
    pending: Option<u32>,
    file: Option<String>,
    err: Option<String>,
    stack: Option<String>,
}

/// `["pass", {…}]`: the kind of event and its test.
fn event(line: &str) -> Option<(String, Event)> {
    let line = line.trim();
    if !line.starts_with("[\"") {
        return None;
    }
    serde_json::from_str::<(String, Event)>(line).ok()
}

struct MochaParser {
    root: PathBuf,
    roots: Vec<PathBuf>,
    /// For each file read: full titles, joined with spaces as mocha joins
    /// them, to test ids.
    names: HashMap<String, HashMap<String, String>>,
}

impl MochaParser {
    fn id(&mut self, file: &str, full_title: &str) -> String {
        let root = &self.root;
        let names = self.names.entry(file.to_owned()).or_insert_with(|| {
            let text = std::fs::read_to_string(root.join(file)).unwrap_or_default();
            parse_js(file, &text)
                .into_iter()
                .map(|t| (spaced(&t.id), t.id))
                .collect()
        });
        names
            .get(full_title)
            .cloned()
            .unwrap_or_else(|| format!("{file}::{full_title}"))
    }
}

impl Parser for MochaParser {
    /// Each test as the spec reporter would show it, not as JSON.
    fn display(&self, line: &str) -> Option<String> {
        let Some((kind, event)) = event(line) else { return Some(line.to_owned()) };
        let title = event.full_title.unwrap_or_default();
        match kind.as_str() {
            "pass" => Some(format!("  ✓ {title}")),
            "fail" => Some(format!("  ✗ {title}")),
            "pending" => Some(format!("  - {title}")),
            "end" => Some(format!(
                "{} passing, {} failing, {} pending",
                event.passes.unwrap_or(0),
                event.failures.unwrap_or(0),
                event.pending.unwrap_or(0)
            )),
            _ => None,
        }
    }

    fn feed(&mut self, line: &str) -> Vec<Found> {
        let Some((kind, event)) = event(line) else {
            return Vec::new();
        };
        let outcome = match kind.as_str() {
            "pass" => Outcome::Passed,
            "fail" => Outcome::Failed,
            "pending" => Outcome::Skipped,
            _ => return Vec::new(),
        };
        let (Some(file), Some(title)) = (
            event.file.as_deref().and_then(|f| relative(&self.roots, f)),
            event.full_title,
        ) else {
            return Vec::new();
        };
        let id = self.id(&file, &title);
        let mut found = Vec::new();
        if outcome == Outcome::Failed {
            let stack = event.stack.or(event.err).unwrap_or_default();
            found.push(Found::Failure(
                id.clone(),
                stack.lines().map(str::to_owned).collect(),
            ));
        }
        found.push(Found::Result(id, outcome));
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::test_support::{failures, parse, results};

    #[test]
    fn reads_mocha_events() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join("test")).unwrap();
        std::fs::write(
            root.join("test/math.spec.js"),
            "describe('math', function () {\n  it('adds', () => {});\n  context('nested group', () => {\n    it('fails here', () => {});\n  });\n});\n",
        )
        .unwrap();
        let file = root.join("test/math.spec.js").display().to_string();
        // From mocha 11's `json-stream` reporter.
        let output = format!(
            r#"["start",{{"total":2}}]
["pass",{{"title":"adds","fullTitle":"math adds","file":"{file}","duration":1,"currentRetry":0,"speed":"fast"}}]
["fail",{{"title":"fails here","fullTitle":"math nested group fails here","file":"{file}","duration":20,"currentRetry":0,"err":"3 == 4","stack":"AssertionError [ERR_ASSERTION]: 3 == 4\n    at Context.<anonymous> (test/math.spec.js:9:14)"}}]
["end",{{"suites":2,"tests":2}}]"#
        );
        let parser = MochaParser {
            root: root.to_path_buf(),
            roots: roots(root),
            names: HashMap::new(),
        };
        let found = parse(Box::new(parser), &output);
        assert_eq!(
            results(&found),
            [
                ("test/math.spec.js::math > adds", Outcome::Passed),
                (
                    "test/math.spec.js::math > nested group > fails here",
                    Outcome::Failed
                ),
            ]
        );
        assert_eq!(
            failures(&found)[0].1,
            [
                "AssertionError [ERR_ASSERTION]: 3 == 4",
                "    at Context.<anonymous> (test/math.spec.js:9:14)"
            ]
        );
    }
}
