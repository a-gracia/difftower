//! Elixir's ExUnit, through `mix test --trace`, which prints a line per
//! test under a header naming its module and file. Tests are known by file
//! and line, read from `test/**/*_test.exs` without compiling.

use std::{collections::HashMap, path::Path};

use anyhow::Result;

use super::{Blocks, Found, Kind, Outcome, Parser, Started, TestCase, TestTarget, file_and_line, target_args, walk};

pub(crate) struct Mix;

impl Kind for Mix {
    fn detect(&self, root: &Path) -> bool {
        root.join("mix.exs").is_file()
    }

    fn default_command(&self, _root: &Path) -> String {
        "mix".to_owned()
    }

    fn discover(&self, root: &Path, _command: &str) -> Result<Vec<TestCase>> {
        Ok(tests(root))
    }

    fn start(&self, root: &Path, command: &str, target: &TestTarget) -> Result<Started> {
        let args = target_args(target, |case| file_and_line(&case.id, case.line));
        let by_line = tests(root)
            .into_iter()
            .filter_map(|t| Some(((t.file.clone(), t.line?), t.id)))
            .collect();
        Ok(Started {
            script: format!("{command} test --trace {args}"),
            parser: Box::new(MixParser { by_line, ..MixParser::default() }),
        })
    }
}

/// Every test under `test/`, and in an umbrella's `apps/*/test/`.
fn tests(root: &Path) -> Vec<TestCase> {
    let files = walk(root, |file| {
        (file.starts_with("test/") || (file.starts_with("apps/") && file.contains("/test/"))) && file.ends_with("_test.exs")
    });
    files
        .iter()
        .flat_map(|file| parse_file(file, &std::fs::read_to_string(root.join(file)).unwrap_or_default()))
        .collect()
}

/// The tests of one file: `test "name"`, inside a `describe "group"` or
/// not. ExUnit names them `group name`; shown as `group > name`.
fn parse_file(file: &str, text: &str) -> Vec<TestCase> {
    let mut tests = Vec::new();
    // The describe block open, with its indentation.
    let mut describe: Option<(usize, String)> = None;
    for (ix, line) in text.lines().enumerate() {
        let indent = line.len() - line.trim_start().len();
        let trimmed = line.trim_start();
        if let Some((level, _)) = &describe
            && indent <= *level
            && (trimmed == "end" || trimmed.starts_with("end "))
        {
            describe = None;
            continue;
        }
        if let Some(name) = trimmed.strip_prefix("describe ").and_then(quoted) {
            describe = Some((indent, name));
            continue;
        }
        let Some(name) = trimmed.strip_prefix("test ").and_then(quoted) else { continue };
        let name = match &describe {
            Some((_, group)) => format!("{group} > {name}"),
            None => name,
        };
        tests.push(TestCase {
            id: format!("{file}::{name}"),
            file: file.to_owned(),
            name,
            line: Some(ix as u32 + 1),
        });
    }
    tests
}

/// The string literal `text` opens with.
fn quoted(text: &str) -> Option<String> {
    let rest = text.trim_start().strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push(chars.next()?),
            '"' => return Some(out),
            c => out.push(c),
        }
    }
    None
}

#[derive(Default)]
struct MixParser {
    /// A test's file and line to its id.
    by_line: HashMap<(String, u32), String>,
    /// The file of the module being run, from its header.
    file: Option<String>,
    /// A test that finished, held back until it is clear whether a failure
    /// block about it follows.
    pending: Option<String>,
    /// A failure header was read; its next line gives the test's place.
    expect_place: bool,
    blocks: Blocks,
}

impl MixParser {
    fn flush(&mut self) -> Vec<Found> {
        self.pending.take().map(|id| Found::Result(id, Outcome::Passed)).into_iter().collect()
    }
}

impl Parser for MixParser {
    fn feed(&mut self, line: &str) -> Vec<Found> {
        // Without a terminal, `* test x [L#4]` and its result share a line,
        // the first meant to be overwritten.
        let line = line.rsplit('\r').next().unwrap_or(line);
        let mut found = Vec::new();
        if self.expect_place {
            self.expect_place = false;
            // `test/shop/cart_test.exs:21`
            let place = line.trim().rsplit_once(':').and_then(|(file, n)| Some((file.to_owned(), n.parse().ok()?)));
            if let Some(id) = place.and_then(|place| self.by_line.get(&place).cloned()) {
                if self.pending.as_ref() == Some(&id) {
                    self.pending = None;
                }
                found.push(Found::Result(id.clone(), Outcome::Failed));
                self.blocks.start(vec![id]);
            }
            self.blocks.push(line.trim_start());
            return found;
        }
        // `  1) test totals is never negative (Shop.CartTest)`
        let trimmed = line.trim_start();
        let header = trimmed.split_once(") test ").is_some_and(|(n, _)| n.parse::<u32>().is_ok());
        if header {
            found.extend(self.blocks.end());
            self.expect_place = true;
            return found;
        }
        if self.blocks.reading() {
            if let Some(rest) = line.strip_prefix("     ") {
                self.blocks.push(rest);
                return found;
            }
            found.extend(self.blocks.end());
        }
        // `Shop.CartTest [test/shop/cart_test.exs]`
        if !line.starts_with(' ')
            && let Some(file) = line.strip_suffix(']').and_then(|l| l.rsplit_once(" [")).map(|(_, file)| file)
        {
            found.extend(self.flush());
            self.file = Some(file.to_owned());
            return found;
        }
        // `  * test adds (0.00ms) [L#4]`, `(skipped)`, `(excluded)`.
        if let Some(rest) = trimmed.strip_prefix("* test ")
            && let Some((head, place)) = rest.rsplit_once(" [L#")
            && let Some(line) = place.strip_suffix(']').and_then(|n| n.parse::<u32>().ok())
            && let Some(close) = head.strip_suffix(')')
            && let Some((_, status)) = close.rsplit_once(" (")
        {
            found.extend(self.flush());
            let Some(id) = self.file.clone().and_then(|file| self.by_line.get(&(file, line)).cloned()) else {
                return found;
            };
            match status {
                "excluded" => {}
                "skipped" | "invalid" => found.push(Found::Result(id, Outcome::Skipped)),
                _ => self.pending = Some(id),
            }
            return found;
        }
        if trimmed.starts_with("Finished in ") {
            found.extend(self.flush());
        }
        found
    }

    fn finish(&mut self) -> Vec<Found> {
        let mut found = self.blocks.end();
        found.extend(self.flush());
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::test_support::{failures, parse, results};

    const FILE: &str = r#"defmodule Shop.CartTest do
  use ExUnit.Case, async: true

  test "adds" do
    assert 1 + 1 == 2
  end

  test "fails with a message" do
    assert 1 + 1 == 3
  end

  @tag :skip
  test "later" do
  end

  describe "totals" do
    test "rounds half up" do
      assert round(2.5) == 3
    end

    test "is never \"negative\"", %{conn: conn} do
      assert -1 > 0, "negative"
    end
  end

  test "after the group" do
  end
end
"#;

    #[test]
    fn reads_exunit_tests() {
        let tests = parse_file("test/shop/cart_test.exs", FILE);
        let found: Vec<_> = tests.iter().map(|t| (t.name.as_str(), t.line)).collect();
        assert_eq!(
            found,
            [
                ("adds", Some(4)),
                ("fails with a message", Some(8)),
                ("later", Some(13)),
                ("totals > rounds half up", Some(17)),
                ("totals > is never \"negative\"", Some(21)),
                ("after the group", Some(26)),
            ]
        );
    }

    #[test]
    fn reads_trace_output() {
        // Captured from `mix test --trace` with Elixir 1.19, no terminal.
        let output = "Running ExUnit with seed: 206572, max_cases: 1


Shop.CartTest [test/shop/cart_test.exs]
  * test later [L#13]\r  * test later (skipped) [L#13]
  * test totals is never \"negative\" [L#21]\r  * test totals is never \"negative\" (10.7ms) [L#21]

  1) test totals is never \"negative\" (Shop.CartTest)
     test/shop/cart_test.exs:21
     negative
     code: assert -1 > 0, \"negative\"
     stacktrace:
       test/shop/cart_test.exs:22: (test)

  * test adds [L#4]\r  * test adds (0.00ms) [L#4]
  * test totals rounds half up [L#17]\r  * test totals rounds half up (excluded) [L#17]
  * test fails with a message [L#8]\r  * test fails with a message (0.03ms) [L#8]

  2) test fails with a message (Shop.CartTest)
     test/shop/cart_test.exs:8
     Assertion with == failed
     code:  assert 1 + 1 == 3
     left:  2
     right: 3
     stacktrace:
       test/shop/cart_test.exs:9: (test)


ShopTest [test/shop_test.exs]
  * doctest Shop.hello/0 (1) [L#3]\r  * doctest Shop.hello/0 (1) (0.00ms) [L#3]

Finished in 0.07 seconds (0.07s async, 0.00s sync)";
        let file = "test/shop/cart_test.exs";
        let tests = parse_file(file, FILE);
        let by_line = tests.into_iter().map(|t| ((t.file.clone(), t.line.unwrap()), t.id)).collect();
        let found = parse(Box::new(MixParser { by_line, ..MixParser::default() }), output);
        let id = |name: &str| format!("{file}::{name}");
        let results: Vec<_> = results(&found).into_iter().map(|(id, o)| (id.to_owned(), o)).collect();
        assert_eq!(
            results,
            [
                (id("later"), Outcome::Skipped),
                (id("totals > is never \"negative\""), Outcome::Failed),
                (id("adds"), Outcome::Passed),
                (id("fails with a message"), Outcome::Failed),
            ]
        );
        let failures = failures(&found);
        assert_eq!(failures.len(), 2);
        assert_eq!(failures[1].0, id("fails with a message"));
        assert_eq!(
            failures[1].1,
            [
                "test/shop/cart_test.exs:8",
                "Assertion with == failed",
                "code:  assert 1 + 1 == 3",
                "left:  2",
                "right: 3",
                "stacktrace:",
                "  test/shop/cart_test.exs:9: (test)"
            ]
        );
    }
}
