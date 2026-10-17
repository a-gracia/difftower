//! Playwright Test, through its `list` reporter.

use std::path::Path;

use anyhow::{Context as _, Result};
use serde::Deserialize;

use super::jest::{json_in, relative, roots};
use super::{
    Blocks, Found, Kind, Outcome, Parser, Started, TestCase, TestTarget, file_and_line,
    shell_output, target_args,
};

pub(crate) struct Playwright;

impl Kind for Playwright {
    fn detect(&self, root: &Path) -> bool {
        ["ts", "js", "mjs", "cjs", "mts", "cts"]
            .iter()
            .any(|ext| root.join(format!("playwright.config.{ext}")).is_file())
    }

    fn default_command(&self, root: &Path) -> String {
        if root.join("node_modules/.bin/playwright").is_file() {
            "node_modules/.bin/playwright".to_owned()
        } else {
            "npx playwright".to_owned()
        }
    }

    fn discover(&self, root: &Path, command: &str) -> Result<Vec<TestCase>> {
        let out = shell_output(root, &format!("{command} test --list --reporter=json"))?;
        let report: Listing =
            json_in(&out).context("unexpected output from playwright test --list")?;
        // Files are relative to the configuration's test folder.
        let base = Path::new(&report.config.root_dir);
        let roots = roots(root);
        let mut tests = Vec::new();
        for suite in &report.suites {
            collect(suite, &[], base, &roots, &mut tests);
        }
        Ok(tests)
    }

    fn start(&self, _root: &Path, command: &str, target: &TestTarget) -> Result<Started> {
        let args = target_args(target, |case| file_and_line(&case.id, case.line));
        Ok(Started {
            script: format!("{command} test --reporter=list {args}"),
            parser: Box::new(PlaywrightParser::default()),
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Listing {
    config: ListedConfig,
    suites: Vec<Suite>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListedConfig {
    root_dir: String,
}

#[derive(Deserialize)]
struct Suite {
    title: String,
    #[serde(default)]
    specs: Vec<Spec>,
    #[serde(default)]
    suites: Vec<Suite>,
}

#[derive(Deserialize)]
struct Spec {
    title: String,
    file: String,
    line: u32,
}

/// The tests of `suite` and the suites in it, under the `describe` blocks
/// `outer`. The top suites are the files: they add no name.
fn collect(
    suite: &Suite,
    outer: &[String],
    base: &Path,
    roots: &[std::path::PathBuf],
    tests: &mut Vec<TestCase>,
) {
    for spec in &suite.specs {
        let path = base.join(&spec.file).display().to_string();
        let Some(file) = relative(roots, &path) else {
            continue;
        };
        let mut names = outer.to_vec();
        names.push(spec.title.clone());
        let name = names.join(" > ");
        tests.push(TestCase {
            id: format!("{file}::{name}"),
            file,
            name,
            line: Some(spec.line),
        });
    }
    for inner in &suite.suites {
        let mut names = outer.to_vec();
        names.push(inner.title.clone());
        collect(inner, &names, base, roots, tests);
    }
}

/// One line per test as it ends, ` ✓  1 [project] › file:3:5 › group ›
/// name (12ms)`, then each failure headed ` 1) [project] › file… ───`.
#[derive(Default)]
struct PlaywrightParser {
    blocks: Blocks,
}

/// `[project] › file:3:5 › group › name`, as a test id; `None` otherwise.
fn title_id(text: &str) -> Option<String> {
    let mut parts: Vec<&str> = text.split(" › ").collect();
    if parts
        .first()
        .is_some_and(|p| p.starts_with('[') && p.ends_with(']'))
    {
        parts.remove(0);
    }
    let (location, names) = parts.split_first()?;
    let mut segments = location.rsplitn(3, ':');
    let (_col, _line, file) = (segments.next()?, segments.next()?, segments.next()?);
    if names.is_empty() || file.is_empty() {
        return None;
    }
    Some(format!("{file}::{}", names.join(" > ")))
}

impl Parser for PlaywrightParser {
    fn feed(&mut self, line: &str) -> Vec<Found> {
        let trimmed = line.trim_start();
        let marks = [
            ('✓', Outcome::Passed),
            ('✘', Outcome::Failed),
            ('-', Outcome::Skipped),
            ('°', Outcome::Skipped),
        ];
        if let Some((rest, outcome)) = marks
            .iter()
            .find_map(|(mark, outcome)| Some((trimmed.strip_prefix(*mark)?, *outcome)))
        {
            // `  1 [project] › …`: the test's number, then its title.
            let rest = rest.trim_start();
            let rest = rest
                .trim_start_matches(|c: char| c.is_ascii_digit())
                .trim_start();
            let rest = rest
                .strip_suffix(')')
                .and_then(|r| r.rsplit_once(" ("))
                .map_or(rest, |(title, _)| title);
            let rest = rest.split(" (retry #").next().unwrap_or(rest);
            if let Some(id) = title_id(rest) {
                return vec![Found::Result(id, outcome)];
            }
        }
        // `  1) [project] › tests/a.spec.ts:8:9 › group › name ─────`.
        if let Some((number, rest)) = trimmed.split_once(") ")
            && line.starts_with("  ") && !line.starts_with("    ") && number.parse::<u32>().is_ok()
            {
                let title = rest.trim_end_matches(['─', ' ']);
                if let Some(id) = title_id(title) {
                    return self.blocks.start(vec![id]);
                }
            }
        // `  1 failed`, `  2 passed (3s)`: the summary closes the last block.
        let words = [
            "failed",
            "flaky",
            "skipped",
            "passed",
            "interrupted",
            "did not run",
        ];
        if line.starts_with("  ") && !line.starts_with("    ")
            && let Some((count, word)) = trimmed.split_once(' ')
                && count.parse::<u32>().is_ok() && words.iter().any(|w| word.starts_with(w)) {
                    return self.blocks.end();
                }
        // A blank line opens each block.
        if self.blocks.has_lines() || !line.trim().is_empty() {
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
    use crate::testing::test_support::{failures, parse, results};

    #[test]
    fn reads_playwright_output() {
        // From Playwright 1.5x's list reporter.
        let output = "\
Running 4 tests using 1 worker

  ✓  1 [unit] › tests/math.spec.ts:4:7 › math › adds (75ms)
  ✘  2 [unit] › tests/math.spec.ts:8:9 › math › nested group › fails here (196ms)
  -  3 [unit] › tests/math.spec.ts:11:10 › math › nested group › is skipped
  ✓  4 tests/other.spec.ts:15:5 › top level (30ms)


  1) [unit] › tests/math.spec.ts:8:9 › math › nested group › fails here ────────────────────────────

    Error: expect(received).toBe(expected) // Object.is equality

    Expected: 4
    Received: 3
        at /srv/app/tests/math.spec.ts:9:21

  1 failed
    [unit] › tests/math.spec.ts:8:9 › math › nested group › fails here ─────────────────────────────
  1 skipped
  2 passed (15.5s)";
        let found = parse(Box::new(PlaywrightParser::default()), output);
        assert_eq!(
            results(&found),
            [
                ("tests/math.spec.ts::math > adds", Outcome::Passed),
                (
                    "tests/math.spec.ts::math > nested group > fails here",
                    Outcome::Failed
                ),
                (
                    "tests/math.spec.ts::math > nested group > is skipped",
                    Outcome::Skipped
                ),
                ("tests/other.spec.ts::top level", Outcome::Passed),
            ]
        );
        let failed = failures(&found);
        assert_eq!(failed.len(), 1);
        assert_eq!(
            failed[0].1[0],
            "    Error: expect(received).toBe(expected) // Object.is equality"
        );
        assert_eq!(
            failed[0].1.last().unwrap(),
            "        at /srv/app/tests/math.spec.ts:9:21"
        );
    }

    #[test]
    fn reads_playwright_listings() {
        let json = r#"{"config":{"rootDir":"/srv/app/tests"},"suites":[{"title":"math.spec.ts","file":"math.spec.ts","specs":[
            {"title":"top level","file":"math.spec.ts","line":15}],"suites":[{"title":"math","file":"math.spec.ts","specs":[
            {"title":"adds","file":"math.spec.ts","line":4}],"suites":[{"title":"nested group","file":"math.spec.ts","specs":[
            {"title":"fails here","file":"math.spec.ts","line":8}]}]}]}]}"#;
        let listing: Listing = json_in(json).unwrap();
        let mut tests = Vec::new();
        for suite in &listing.suites {
            collect(
                suite,
                &[],
                Path::new(&listing.config.root_dir),
                &[std::path::PathBuf::from("/srv/app")],
                &mut tests,
            );
        }
        let ids: Vec<_> = tests.iter().map(|t| (t.id.as_str(), t.line)).collect();
        assert_eq!(
            ids,
            [
                ("tests/math.spec.ts::top level", Some(15)),
                ("tests/math.spec.ts::math > adds", Some(4)),
                (
                    "tests/math.spec.ts::math > nested group > fails here",
                    Some(8)
                ),
            ]
        );
    }
}
