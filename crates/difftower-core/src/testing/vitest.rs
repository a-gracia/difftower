//! Vitest.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde::Deserialize;

use super::{
    Blocks, Found, Kind, Outcome, Parser, Started, TestCase, TestTarget, file_and_line, read, shell_output,
    target_args,
};

pub(crate) struct Vitest;

impl Kind for Vitest {
    fn detect(&self, root: &Path) -> bool {
        let config = ["vitest.config.ts", "vitest.config.js", "vitest.config.mts", "vitest.config.mjs"]
            .iter()
            .any(|name| root.join(name).is_file());
        config || read(root, "package.json").contains("\"vitest\"")
    }

    fn default_command(&self, root: &Path) -> String {
        if root.join("node_modules/.bin/vitest").is_file() {
            "node_modules/.bin/vitest".to_owned()
        } else {
            "npx vitest".to_owned()
        }
    }

    fn discover(&self, root: &Path, command: &str) -> Result<Vec<TestCase>> {
        let out = shell_output(root, &format!("{command} list --json"))?;
        parse_list(&out, root)
    }

    fn start(&self, _root: &Path, command: &str, target: &TestTarget) -> Result<Started> {
        let args = target_args(target, |case| file_and_line(&case.id, case.line));
        Ok(Started {
            script: format!("{command} run --reporter=verbose {args}"),
            parser: Box::new(VitestParser::default()),
        })
    }
}

#[derive(Deserialize)]
struct Listed {
    name: String,
    file: String,
    location: Option<Location>,
}

#[derive(Deserialize)]
struct Location {
    line: u32,
}

fn parse_list(out: &str, root: &Path) -> Result<Vec<TestCase>> {
    let start = out.find('[').context("vitest listed no tests")?;
    let end = out.rfind(']').context("vitest listed no tests")?;
    let listed: Vec<Listed> = serde_json::from_str(&out[start..=end]).context("unexpected output from vitest list")?;
    let roots: Vec<PathBuf> = [Some(root.to_path_buf()), root.canonicalize().ok()].into_iter().flatten().collect();
    Ok(listed
        .into_iter()
        .map(|t| {
            let path = Path::new(&t.file);
            let file = roots
                .iter()
                .find_map(|root| path.strip_prefix(root).ok())
                .map_or(t.file.clone(), |p| p.display().to_string());
            TestCase { id: format!("{file}::{}", t.name), file, name: t.name, line: t.location.map(|l| l.line) }
        })
        .collect())
}

/// ` ✓ src/a.test.ts > group > name 12ms`, `×` when it failed and `↓`
/// when skipped.
fn parse_result(line: &str) -> Option<(String, Outcome)> {
    let line = line.trim_start();
    let mut chars = line.chars();
    let outcome = match chars.next()? {
        '✓' | '√' => Outcome::Passed,
        '×' | '✗' => Outcome::Failed,
        '↓' => Outcome::Skipped,
        _ => return None,
    };
    let rest = chars.as_str().trim_start();
    let (file, name) = rest.split_once(" > ")?;
    // Run by `file:line`, vitest names the file that way.
    let file = match file.rsplit_once(':') {
        Some((path, line)) if line.parse::<u32>().is_ok() => path,
        _ => file,
    };
    Some((format!("{file}::{}", strip_duration(name)), outcome))
}

/// `name` without a trailing duration: ` 12ms` or ` 1.2s`.
pub(crate) fn strip_duration(name: &str) -> &str {
    match name.rsplit_once(' ') {
        Some((head, tail)) if tail.ends_with('s') && tail.trim_end_matches(['m', 's']).parse::<f64>().is_ok() => head,
        _ => name,
    }
}

/// ` FAIL  file > group > name` headers, several for one shared error, up
/// to a `⎯⎯⎯` rule.
#[derive(Default)]
struct VitestParser {
    blocks: Blocks,
}

impl Parser for VitestParser {
    fn feed(&mut self, line: &str) -> Vec<Found> {
        let mut found: Vec<Found> = parse_result(line).map(|(id, o)| Found::Result(id, o)).into_iter().collect();
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("FAIL ") {
            if let Some((file, name)) = rest.trim().split_once(" > ") {
                let id = format!("{file}::{name}");
                if self.blocks.has_lines() || !self.blocks.reading() {
                    found.extend(self.blocks.start(vec![id]));
                } else {
                    self.blocks.also(id);
                }
            }
            return found;
        }
        if trimmed.starts_with('⎯') || trimmed.starts_with("Test Files ") {
            found.extend(self.blocks.end());
            return found;
        }
        self.blocks.push(line);
        found
    }

    fn finish(&mut self) -> Vec<Found> {
        self.blocks.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{
        strip_ansi,
        test_support::{failures, parse},
    };

    #[test]
    fn reads_vitest_output() {
        let listed = r#"[{"name":"group > adds","file":"/srv/app/src/a.test.ts","location":{"line":34,"column":3}}]"#;
        let tests = parse_list(listed, Path::new("/srv/app")).unwrap();
        assert_eq!(
            tests,
            [TestCase {
                id: "src/a.test.ts::group > adds".into(),
                file: "src/a.test.ts".into(),
                name: "group > adds".into(),
                line: Some(34)
            }]
        );
        let colored = strip_ansi(" \u{1b}[32m✓\u{1b}[39m src/a.test.ts\u{1b}[2m > \u{1b}[22mgroup\u{1b}[2m > \u{1b}[22madds\u{1b}[32m 12\u{1b}[2mms\u{1b}[22m\u{1b}[39m");
        assert_eq!(parse_result(&colored), Some(("src/a.test.ts::group > adds".into(), Outcome::Passed)));
        assert_eq!(
            parse_result(" × src/a.test.ts > group > takes 2 s 3ms"),
            Some(("src/a.test.ts::group > takes 2 s".into(), Outcome::Failed))
        );
        assert_eq!(parse_result(" ✓ src/a.test.ts (17 tests) 30ms"), None);
        assert_eq!(
            parse_result(" ↓ src/a.test.ts:12 > group > adds"),
            Some(("src/a.test.ts::group > adds".into(), Outcome::Skipped))
        );
    }

    #[test]
    fn reads_vitest_failures() {
        let output = "\
⎯⎯⎯⎯⎯⎯⎯ Failed Tests 2 ⎯⎯⎯⎯⎯⎯⎯

 FAIL  src/a.test.ts > group > adds
 FAIL  src/a.test.ts > group > adds again
AssertionError: expected 3 to be 4
 ❯ src/a.test.ts:5:15

⎯⎯⎯⎯⎯⎯⎯[1/1]⎯

 Test Files  1 failed (1)";
        let found = parse(Box::new(VitestParser::default()), output);
        let found = failures(&found);
        let ids: Vec<_> = found.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, ["src/a.test.ts::group > adds", "src/a.test.ts::group > adds again"]);
        assert_eq!(found[0].1, ["AssertionError: expected 3 to be 4", " ❯ src/a.test.ts:5:15"]);
    }
}
