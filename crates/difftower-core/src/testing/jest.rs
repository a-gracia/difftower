//! Jest, read through its JSON report. Also home to what the JavaScript
//! runners share: reading the tests a file declares.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result};
use serde::Deserialize;

use super::{
    Found, Kind, Outcome, Parser, Started, TestCase, TestTarget, file_of, quote, read,
    regex_escape, report_path, shell_output, strip_ansi, target_args, walk,
};

pub(crate) struct Jest;

impl Kind for Jest {
    fn detect(&self, root: &Path) -> bool {
        let config = [
            "jest.config.js",
            "jest.config.ts",
            "jest.config.mjs",
            "jest.config.cjs",
            "jest.config.json",
        ]
        .iter()
        .any(|name| root.join(name).is_file());
        // A dependency, a `"jest": {…}` config or a `"test": "jest"` script.
        config || read(root, "package.json").contains("\"jest\"")
    }

    fn default_command(&self, root: &Path) -> String {
        if root.join("node_modules/.bin/jest").is_file() {
            "node_modules/.bin/jest".to_owned()
        } else {
            "npx jest".to_owned()
        }
    }

    fn discover(&self, root: &Path, command: &str) -> Result<Vec<TestCase>> {
        let out = shell_output(root, &format!("{command} --listTests"))?;
        let roots = roots(root);
        let files: Vec<String> = out
            .lines()
            .filter_map(|line| relative(&roots, line.trim()))
            .filter(|f| root.join(f).is_file())
            .collect();
        Ok(js_tests(root, &files))
    }

    fn start(&self, root: &Path, command: &str, target: &TestTarget) -> Result<Started> {
        let report = report_path("jest.json");
        let args = match target {
            TestTarget::Cases { cases } => {
                // The files, and the tests by full name: jest joins the
                // describe blocks and the test with spaces.
                let mut files: Vec<String> =
                    cases.iter().map(|c| file_of(&c.id).to_owned()).collect();
                files.dedup();
                let names: Vec<String> =
                    cases.iter().map(|c| regex_escape(&spaced(&c.id))).collect();
                let files = files.iter().map(|f| quote(f)).collect::<Vec<_>>().join(" ");
                format!("{files} -t {}", quote(&format!("^({})$", names.join("|"))))
            }
            _ => target_args(target, |case| case.id.clone()),
        };
        Ok(Started {
            script: format!(
                "{command} --ci --json --testLocationInResults --outputFile={} {args}",
                quote(&report.display().to_string())
            ),
            parser: Box::new(JestParser {
                roots: roots(root),
                report,
                only: only(target),
            }),
        })
    }
}

/// `file::a > b > c` as a full name joined with spaces, as jest, bun, node
/// and mocha match names.
pub(crate) fn spaced(id: &str) -> String {
    id.split_once("::")
        .map_or(id, |(_, name)| name)
        .split(" > ")
        .collect::<Vec<_>>()
        .join(" ")
}

/// `root`, and the same directory with links resolved: runners print either.
pub(crate) fn roots(root: &Path) -> Vec<PathBuf> {
    [Some(root.to_path_buf()), root.canonicalize().ok()]
        .into_iter()
        .flatten()
        .collect()
}

/// `path` relative to one of `roots`, if it is under one; a relative path
/// as it is, without a leading `./`.
pub(crate) fn relative(roots: &[PathBuf], path: &str) -> Option<String> {
    let path = path.strip_prefix("file://").unwrap_or(path);
    if path.is_empty() {
        return None;
    }
    if !path.starts_with('/') {
        return Some(path.strip_prefix("./").unwrap_or(path).to_owned());
    }
    roots
        .iter()
        .find_map(|root| Path::new(path).strip_prefix(root).ok())
        .map(|p| p.display().to_string())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Report {
    test_results: Vec<FileResult>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileResult {
    name: String,
    assertion_results: Vec<Assertion>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Assertion {
    ancestor_titles: Vec<String>,
    title: String,
    status: String,
    #[serde(default)]
    failure_messages: Vec<String>,
}

struct JestParser {
    roots: Vec<PathBuf>,
    report: PathBuf,
    /// When running some tests, those: jest reports the rest of their
    /// files as skipped.
    only: Option<HashSet<String>>,
}

/// The ids a run of `target` is about, when it is about some tests only.
pub(crate) fn only(target: &TestTarget) -> Option<HashSet<String>> {
    match target {
        TestTarget::Cases { cases } => Some(cases.iter().map(|c| c.id.clone()).collect()),
        _ => None,
    }
}

impl Parser for JestParser {
    fn feed(&mut self, _line: &str) -> Vec<Found> {
        Vec::new()
    }

    fn finish(&mut self) -> Vec<Found> {
        let text = std::fs::read_to_string(&self.report).unwrap_or_default();
        let _ = std::fs::remove_file(&self.report);
        let Ok(report) = serde_json::from_str::<Report>(&text) else {
            return Vec::new();
        };
        let mut found = Vec::new();
        for file in report.test_results {
            let Some(path) = relative(&self.roots, &file.name) else {
                continue;
            };
            for test in file.assertion_results {
                let mut names = test.ancestor_titles;
                names.push(test.title);
                let id = format!("{path}::{}", names.join(" > "));
                if self.only.as_ref().is_some_and(|only| !only.contains(&id)) {
                    continue;
                }
                let outcome = match test.status.as_str() {
                    "passed" => Outcome::Passed,
                    "failed" => Outcome::Failed,
                    _ => Outcome::Skipped,
                };
                if outcome == Outcome::Failed {
                    let lines = test
                        .failure_messages
                        .iter()
                        .flat_map(|m| strip_ansi(m).lines().map(str::to_owned).collect::<Vec<_>>())
                        .collect();
                    found.push(Found::Failure(id.clone(), lines));
                }
                found.push(Found::Result(id, outcome));
            }
        }
        found
    }
}

/// The files under `root` named as test files are: `a.test.ts`,
/// `a.spec.js`, `a_test.mjs`, or any script in a `__tests__` folder.
pub(crate) fn test_files(root: &Path) -> Vec<String> {
    walk(root, |file| {
        is_script(file) && (is_test_name(file) || file.contains("__tests__/"))
    })
}

pub(crate) fn is_script(file: &str) -> bool {
    const EXTS: [&str; 8] = [".js", ".jsx", ".ts", ".tsx", ".mjs", ".cjs", ".mts", ".cts"];
    EXTS.iter().any(|ext| file.ends_with(ext))
}

/// `a.test.ts`, `a.spec.js`, `a_test.ts`, `a-test.js` or `a_spec.ts`.
pub(crate) fn is_test_name(file: &str) -> bool {
    let name = file.rsplit('/').next().unwrap_or(file);
    let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
    [".test", ".spec", "_test", "-test", "_spec"]
        .iter()
        .any(|suffix| stem.ends_with(suffix))
}

/// The tests declared in `files`, read from them.
pub(crate) fn js_tests(root: &Path, files: &[String]) -> Vec<TestCase> {
    files
        .iter()
        .flat_map(|file| {
            parse_js(
                file,
                &std::fs::read_to_string(root.join(file)).unwrap_or_default(),
            )
        })
        .collect()
}

/// Whether a test file at `root` mentions `needle`, reading only the first
/// files found: a cheap check for an import such as `bun:test`.
pub(crate) fn some_test_mentions(root: &Path, needle: &str) -> bool {
    test_files(root)
        .iter()
        .take(50)
        .any(|file| read(root, file).contains(needle))
}

/// The tests a JavaScript or TypeScript file declares with `describe`,
/// `it`, `test` and their kin (`context`, `suite`, `specify`, `Deno.test`,
/// `t.step` and `t.test` inside a test), named as the runners name them:
/// `group > nested > test`. Read without running anything, so names built
/// at run time, as with `test.each`, are missed.
pub(crate) fn parse_js(file: &str, text: &str) -> Vec<TestCase> {
    struct Scope {
        /// Brace depth at the call.
        depth: usize,
        name: String,
        /// Its body has opened.
        opened: bool,
        test: bool,
    }
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let (mut i, mut depth, mut line) = (0, 0usize, 1u32);
    let mut scopes: Vec<Scope> = Vec::new();
    let mut tests = Vec::new();
    let is_ident = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    while i < n {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        match c {
            '\n' => line += 1,
            '/' if next == Some('/') => {
                while i < n && chars[i] != '\n' {
                    i += 1;
                }
                continue;
            }
            '/' if next == Some('*') => {
                i += 2;
                while i < n && !(chars[i] == '*' && chars.get(i + 1) == Some(&'/')) {
                    line += u32::from(chars[i] == '\n');
                    i += 1;
                }
                i += 2;
                continue;
            }
            '"' | '\'' | '`' => {
                let (_, end) = skip_string(&chars, i, &mut line);
                i = end;
                continue;
            }
            '{' => {
                if let Some(last) = scopes.last_mut().filter(|s| !s.opened && s.depth == depth) {
                    last.opened = true;
                }
                depth += 1;
            }
            '}' => {
                depth = depth.saturating_sub(1);
                scopes.retain(|s| !(s.opened && depth <= s.depth));
            }
            c if is_ident(c)
                && !c.is_ascii_digit()
                && (i == 0 || !(is_ident(chars[i - 1]) || chars[i - 1] == '.')) =>
            {
                // A chain such as `describe.only` or `Deno.test`.
                let start_line = line;
                let mut parts = Vec::new();
                let mut j = i;
                loop {
                    let begin = j;
                    while j < n && is_ident(chars[j]) {
                        j += 1;
                    }
                    parts.push(chars[begin..j].iter().collect::<String>());
                    if j < n && chars[j] == '.' && chars.get(j + 1).is_some_and(|c| is_ident(*c)) {
                        j += 1;
                    } else {
                        break;
                    }
                }
                i = j;
                let mut k = j;
                while k < n && chars[k].is_whitespace() {
                    k += 1;
                }
                if k >= n || chars[k] != '(' {
                    continue;
                }
                let in_test = scopes.iter().any(|s| s.test);
                let Some(test) = classify(&parts, in_test) else {
                    continue;
                };
                k += 1;
                while k < n && chars[k].is_whitespace() {
                    k += 1;
                }
                let name = match chars.get(k) {
                    Some('"' | '\'' | '`') => {
                        let mut ignored = line;
                        skip_string(&chars, k, &mut ignored).0
                    }
                    // `Deno.test({ name: "…", fn() {} })`.
                    Some('{') if parts[0] == "Deno" => {
                        let ahead: String = chars[k..n.min(k + 400)].iter().collect();
                        ahead.find("name").and_then(|at| {
                            let rest = ahead[at + 4..].trim_start().strip_prefix(':')?.trim_start();
                            rest.starts_with(['"', '\'', '`']).then_some(())?;
                            let rest: Vec<char> = rest.chars().collect();
                            skip_string(&rest, 0, &mut 0).0
                        })
                    }
                    _ => None,
                };
                let Some(name) = name else { continue };
                // Calls at this depth whose body never opened are over.
                scopes.retain(|s| s.opened || s.depth < depth);
                if test {
                    let mut names: Vec<&str> = scopes.iter().map(|s| s.name.as_str()).collect();
                    names.push(&name);
                    let full = names.join(" > ");
                    tests.push(TestCase {
                        id: format!("{file}::{full}"),
                        file: file.to_owned(),
                        name: full,
                        line: Some(start_line),
                    });
                }
                scopes.push(Scope {
                    depth,
                    name,
                    opened: false,
                    test,
                });
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    tests
}

/// Whether a call chain declares a test (`Some(true)`), a group
/// (`Some(false)`), or neither.
fn classify(parts: &[String], in_test: bool) -> Option<bool> {
    const MODIFIERS: [&str; 12] = [
        "only",
        "skip",
        "todo",
        "concurrent",
        "failing",
        "serial",
        "parallel",
        "fixme",
        "ignore",
        "sequential",
        "fail",
        "slow",
    ];
    let (head, rest): (&[String], &[String]) = match parts {
        [deno, test, rest @ ..] if deno == "Deno" && test == "test" => {
            return rest
                .iter()
                .all(|m| MODIFIERS.contains(&m.as_str()))
                .then_some(true);
        }
        // Playwright's `test.describe`.
        [test, describe, rest @ ..] if test == "test" && describe == "describe" => {
            return rest
                .iter()
                .all(|m| MODIFIERS.contains(&m.as_str()))
                .then_some(false);
        }
        // `t.step` and `t.test` inside a test: Deno's steps, node's subtests.
        [_, sub] if in_test && (sub == "step" || sub == "test") => return Some(true),
        [head, rest @ ..] => (std::slice::from_ref(head), rest),
        [] => return None,
    };
    if !rest.iter().all(|m| MODIFIERS.contains(&m.as_str())) {
        return None;
    }
    match head[0].as_str() {
        "describe" | "context" | "suite" | "fdescribe" | "xdescribe" => Some(false),
        "it" | "test" | "specify" | "fit" | "xit" | "xtest" => Some(true),
        _ => None,
    }
}

/// The string literal opening at `start`, and the index after it. A
/// template literal with `${…}` in it has no fixed value: `None`.
fn skip_string(chars: &[char], start: usize, line: &mut u32) -> (Option<String>, usize) {
    let quote = chars[start];
    let mut value = String::new();
    let mut fixed = true;
    let mut i = start + 1;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\\' => {
                if let Some(&escaped) = chars.get(i + 1) {
                    *line += u32::from(escaped == '\n');
                    value.push(escaped);
                }
                i += 2;
                continue;
            }
            c if c == quote => return (fixed.then_some(value), i + 1),
            '\n' if quote != '`' => return (None, i),
            '$' if quote == '`' && chars.get(i + 1) == Some(&'{') => {
                fixed = false;
                // Up to the matching brace.
                let mut depth = 0;
                while i < chars.len() {
                    match chars[i] {
                        '{' => depth += 1,
                        '}' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        '\n' => *line += 1,
                        _ => {}
                    }
                    i += 1;
                }
            }
            '\n' => *line += 1,
            _ => value.push(c),
        }
        i += 1;
    }
    (None, i)
}

/// Reads a JSON value from the first `{` of `out`, skipping what a runner
/// prints before it.
pub(crate) fn json_in<T: serde::de::DeserializeOwned>(out: &str) -> Result<T> {
    let start = out.find('{').context("no JSON in the output")?;
    let mut stream = serde_json::Deserializer::from_str(&out[start..]).into_iter::<T>();
    stream
        .next()
        .context("no JSON in the output")?
        .context("unexpected JSON")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::test_support::{failures, results};

    #[test]
    fn reads_the_tests_a_file_declares() {
        let text = r#"
import { describe, it, test } from "vitest";
// it("commented out", () => {});
describe("math", () => {
  test("adds", () => {
    const s = "{ not a brace";
    expect(add(1, 2)).toBe(3);
  });
  describe.skip('nested group', function () {
    it.only(`fails here`, () => {});
    it.each([1, 2])("case %i", () => {});
    it(`with ${x} inside`, () => {});
  });
  it("after the group", () => {});
});

test("top level", fn);
test("next to it", fn);
Deno.test({ name: "object form", fn() {} });
Deno.test("with steps", async (t) => {
  await t.step("a step", () => {});
});
"#;
        let tests = parse_js("src/a.test.ts", text);
        let names: Vec<(&str, Option<u32>)> =
            tests.iter().map(|t| (t.name.as_str(), t.line)).collect();
        assert_eq!(
            names,
            [
                ("math > adds", Some(5)),
                ("math > nested group > fails here", Some(10)),
                ("math > after the group", Some(14)),
                ("top level", Some(17)),
                ("next to it", Some(18)),
                ("object form", Some(19)),
                ("with steps", Some(20)),
                ("with steps > a step", Some(21)),
            ]
        );
        assert_eq!(
            tests[1].id,
            "src/a.test.ts::math > nested group > fails here"
        );
        // `regex.test(s)` is not a test.
        assert!(parse_js("a.test.js", "if (/x/.test('y')) {}\nre.test('z');").is_empty());
    }

    #[test]
    fn names_match_the_way_runners_join_them() {
        assert_eq!(
            spaced("src/a.test.ts::math > nested group > fails here"),
            "math nested group fails here"
        );
        assert!(
            is_test_name("src/a.test.ts")
                && is_test_name("a_test.ts")
                && is_test_name("a.spec.mjs")
        );
        assert!(!is_test_name("src/latest.ts"));
    }

    #[test]
    fn reads_jest_reports() {
        let dir = tempfile::tempdir().unwrap();
        let report = dir.path().join("r.json");
        // Trimmed from a real `jest --json --testLocationInResults` report.
        let json = r#"{"numFailedTests":1,"testResults":[{"name":"/srv/app/src/math.test.js","status":"failed","message":"","assertionResults":[
            {"ancestorTitles":["math"],"fullName":"math adds","status":"passed","title":"adds","failureMessages":[],"location":{"column":3,"line":4}},
            {"ancestorTitles":["math","nested group"],"fullName":"math nested group fails here","status":"failed","title":"fails here","failureMessages":["Error: expect(received).toBe(expected) // Object.is equality\n\nExpected: 4\nReceived: 3\n    at Object.toBe (/srv/app/src/math.test.js:9:25)"],"location":{"column":5,"line":8}},
            {"ancestorTitles":["math","nested group"],"status":"pending","title":"is skipped","failureMessages":[]}]}]}"#;
        std::fs::write(&report, json).unwrap();
        let mut parser = JestParser {
            roots: vec![PathBuf::from("/srv/app")],
            report: report.clone(),
            only: None,
        };
        let found = parser.finish();
        assert_eq!(
            results(&found),
            [
                ("src/math.test.js::math > adds", Outcome::Passed),
                (
                    "src/math.test.js::math > nested group > fails here",
                    Outcome::Failed
                ),
                (
                    "src/math.test.js::math > nested group > is skipped",
                    Outcome::Skipped
                ),
            ]
        );
        let failed = failures(&found);
        assert_eq!(
            failed[0].1[0],
            "Error: expect(received).toBe(expected) // Object.is equality"
        );
        assert!(!report.exists(), "the report is removed once read");
    }
}
