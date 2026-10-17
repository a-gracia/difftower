//! `dotnet test`, for xUnit, NUnit and MSTest. Tests are read from C# and
//! F# sources without building; results come live from the console logger
//! and, exactly, from the TRX reports each test project writes.

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use anyhow::Result;

use super::{
    Found, Kind, Outcome, Parser, Started, TestCase, TestTarget,
    gradle::{JvmParser, JvmTest, classes_under, selected},
    quote, report_path, walk,
};

pub(crate) struct Dotnet;

const PROJECTS: [&str; 2] = [".csproj", ".fsproj"];
const TEST_SDKS: [&str; 5] = ["Microsoft.NET.Test.Sdk", "xunit", "nunit", "MSTest", "TUnit"];

impl Kind for Dotnet {
    fn detect(&self, root: &Path) -> bool {
        !test_projects(root).is_empty()
    }

    fn default_command(&self, _root: &Path) -> String {
        "dotnet".to_owned()
    }

    fn discover(&self, root: &Path, _command: &str) -> Result<Vec<TestCase>> {
        Ok(dotnet_tests(root).into_iter().map(|t| t.case).collect())
    }

    fn start(&self, root: &Path, command: &str, target: &TestTarget) -> Result<Started> {
        let tests = dotnet_tests(root);
        let filter = match target {
            TestTarget::All => None,
            // The class, and the classes nested in it.
            TestTarget::Paths { .. } => Some(
                classes_under(&tests, target)
                    .iter()
                    .map(|class| class.split('+').next().unwrap_or(class))
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .flat_map(|class| {
                        let class = escape(class);
                        [format!("FullyQualifiedName~{class}."), format!("FullyQualifiedName~{class}+")]
                    })
                    .collect::<Vec<_>>(),
            ),
            // The method, alone or with the arguments of a theory.
            TestTarget::Cases { .. } => Some(
                selected(&tests, target)
                    .flat_map(|t| {
                        let name = escape(&format!("{}.{}", t.class, t.method));
                        [format!("FullyQualifiedName={name}"), format!("FullyQualifiedName~{name}\\(")]
                    })
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect(),
            ),
        };
        let filter = match filter {
            None => String::new(),
            Some(clauses) if clauses.is_empty() => " --filter FullyQualifiedName=DifftowerNoSuchTest".to_owned(),
            Some(clauses) => format!(" --filter {}", quote(&clauses.join("|"))),
        };
        let results = report_path("dotnet-results");
        let args = format!(
            "test --logger {} --logger trx --results-directory {}{filter}",
            quote("console;verbosity=normal"),
            quote(&results.display().to_string())
        );
        // `dotnet test` finds a solution or project at the root by itself;
        // otherwise each test project runs in turn.
        let at_root = std::fs::read_dir(root).into_iter().flatten().flatten().any(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            name.ends_with(".sln") || name.ends_with(".slnx") || PROJECTS.iter().any(|ext| name.ends_with(ext))
        });
        let script = if at_root {
            format!("{command} {args}")
        } else {
            let runs: Vec<String> =
                test_projects(root).iter().map(|p| format!("{command} {args} {} || ok=1", quote(p))).collect();
            format!("ok=0; {}; exit $ok", runs.join("; "))
        };
        Ok(Started { script, parser: Box::new(DotnetParser::new(&tests, results)) })
    }
}

/// A character that means something in a `--filter` expression, escaped.
fn escape(text: &str) -> String {
    text.chars().fold(String::new(), |mut out, c| {
        if "\\()&|=!~".contains(c) {
            out.push('\\');
        }
        out.push(c);
        out
    })
}

/// The test projects at `root`, in it and in folders up to two levels down,
/// as most layouts keep them: a project that references a test framework.
fn test_projects(root: &Path) -> Vec<String> {
    let mut dirs = vec![(root.to_path_buf(), 0)];
    let mut projects = Vec::new();
    while let Some((dir, level)) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                if level < 2 && !name.starts_with('.') && !["bin", "obj", "node_modules"].contains(&name.as_str()) {
                    dirs.push((path, level + 1));
                }
            } else if PROJECTS.iter().any(|ext| name.ends_with(ext)) {
                let text = std::fs::read_to_string(&path).unwrap_or_default();
                if TEST_SDKS.iter().any(|sdk| text.contains(&format!("\"{sdk}")))
                    && let Ok(rel) = path.strip_prefix(root)
                {
                    projects.push(rel.display().to_string());
                }
            }
        }
    }
    projects.sort();
    projects
}

/// Every test in the C# and F# sources of the repository.
fn dotnet_tests(root: &Path) -> Vec<JvmTest> {
    let files = walk(root, |file| {
        let built = file.split('/').any(|part| part == "bin" || part == "obj");
        !built && (file.ends_with(".cs") || file.ends_with(".fs"))
    });
    files
        .iter()
        .flat_map(|file| {
            let text = std::fs::read_to_string(root.join(file)).unwrap_or_default();
            if file.ends_with(".fs") { parse_fsharp(file, &text) } else { parse_csharp(file, &text) }
        })
        .collect()
}

const TEST_ATTRIBUTES: [&str; 9] =
    ["Fact", "Theory", "Test", "TestCase", "TestCaseSource", "TestMethod", "DataTestMethod", "SkippableFact", "SkippableTheory"];

/// The attributes leading `code`, `[Fact]` or `[Theory, Trait("a", "b")]`,
/// and what follows them.
fn split_attributes(code: &str) -> (Vec<String>, &str) {
    let mut rest = code.trim_start();
    let mut names = Vec::new();
    while rest.starts_with('[') && !rest.starts_with("[<") {
        let mut level = 0;
        let mut end = rest.len();
        for (ix, c) in rest.char_indices() {
            match c {
                '[' | '(' => level += 1,
                ']' | ')' => {
                    level -= 1;
                    if level == 0 && c == ']' {
                        end = ix + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        // Several attributes in one pair of brackets, each maybe with
        // arguments.
        let inside = &rest[1..end.saturating_sub(1)];
        let mut level = 0;
        let mut start = 0;
        for (ix, c) in inside.char_indices().chain([(inside.len(), ',')]) {
            match c {
                '(' => level += 1,
                ')' => level -= 1,
                ',' if level == 0 => {
                    let part = inside[start..ix].trim();
                    let name: String = part.chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.').collect();
                    let name = name.rsplit('.').next().unwrap_or("").trim_end_matches("Attribute").to_owned();
                    names.push(name);
                    start = ix + 1;
                }
                _ => {}
            }
        }
        rest = rest[end..].trim_start();
    }
    (names, rest)
}

/// The tests of one C# file: methods with an xUnit, NUnit or MSTest
/// attribute, in classes nested any depth, named as .NET names them:
/// `Shop.Tests.CartTests+Nested`.
fn parse_csharp(file: &str, text: &str) -> Vec<JvmTest> {
    let stem = file.rsplit('/').next().unwrap_or(file).split('.').next().unwrap_or(file);
    // Enclosing namespaces and classes, with the brace depth inside each.
    let mut scopes: Vec<(String, bool, usize)> = Vec::new();
    let mut file_namespace = String::new();
    let mut pending: Option<(String, bool)> = None;
    let mut pending_test = false;
    let mut depth = 0usize;
    let mut tests = Vec::new();
    for (ix, raw) in text.lines().enumerate() {
        let line = raw.split("//").next().unwrap_or(raw);
        let (attributes, code) = split_attributes(line);
        if attributes.iter().any(|a| TEST_ATTRIBUTES.contains(&a.as_str())) {
            pending_test = true;
        }
        let words: Vec<&str> = code.split_whitespace().collect();
        let ident = |word: Option<&&str>| -> String {
            word.map(|w| w.chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.').collect())
                .unwrap_or_default()
        };
        if let Some(at) = words.iter().position(|w| *w == "namespace") {
            let name = ident(words.get(at + 1));
            if code.trim_end().ends_with(';') {
                file_namespace = name;
            } else {
                pending = Some((name, true));
            }
        } else if let Some(at) = words.iter().position(|w| matches!(*w, "class" | "record" | "struct")) {
            let name = ident(words.get(at + 1));
            if !name.is_empty() {
                pending = Some((name, false));
                pending_test = false;
            }
        } else if pending_test && let Some(method) = csharp_method(code) {
            pending_test = false;
            let namespace: Vec<&str> = std::iter::once(file_namespace.as_str())
                .chain(scopes.iter().filter(|(_, ns, _)| *ns).map(|(name, _, _)| name.as_str()))
                .filter(|n| !n.is_empty())
                .collect();
            let classes: Vec<&str> = scopes.iter().filter(|(_, ns, _)| !*ns).map(|(name, _, _)| name.as_str()).collect();
            if !classes.is_empty() {
                let mut class = classes.join("+");
                if !namespace.is_empty() {
                    class = format!("{}.{class}", namespace.join("."));
                }
                tests.push(test(file, stem, class, &classes, method, ix));
            }
        }
        for c in super::gradle::braces(line) {
            if c == '{' {
                depth += 1;
                if let Some((name, ns)) = pending.take() {
                    scopes.push((name, ns, depth));
                }
            } else {
                scopes.retain(|(_, _, level)| *level < depth);
                depth = depth.saturating_sub(1);
            }
        }
    }
    tests
}

/// The test case for `method` of `class`, whose nested names are `classes`.
fn test(file: &str, stem: &str, class: String, classes: &[&str], method: String, ix: usize) -> JvmTest {
    // The file's own class goes without saying.
    let shown = if classes[0] == stem { &classes[1..] } else { classes };
    let name = shown.iter().copied().chain([method.as_str()]).collect::<Vec<_>>().join(" > ");
    JvmTest {
        class,
        method,
        case: TestCase { id: format!("{file}::{name}"), file: file.to_owned(), name, line: Some(ix as u32 + 1) },
    }
}

/// The method declared on `code`: `public async Task Adds(int n)`.
fn csharp_method(code: &str) -> Option<String> {
    let open = code.find('(')?;
    let head = code[..open].trim_end();
    // A generic method: `Adds<T>`.
    let head = head.strip_suffix('>').map_or(head, |h| h.rsplit_once('<').map_or(h, |(name, _)| name));
    let name: String = head.chars().rev().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
    let name: String = name.chars().rev().collect();
    (!name.is_empty() && head.split_whitespace().count() >= 2).then_some(name)
}

/// The tests of one F# file: ``[<Fact>] let ``adds two`` () =`` in a
/// module, or members of a type.
fn parse_fsharp(file: &str, text: &str) -> Vec<JvmTest> {
    let stem = file.rsplit('/').next().unwrap_or(file).split('.').next().unwrap_or(file);
    let mut namespace = String::new();
    let mut container = String::new();
    let mut pending_test = false;
    let mut tests = Vec::new();
    for (ix, raw) in text.lines().enumerate() {
        let line = raw.split("//").next().unwrap_or(raw).trim();
        if let Some(rest) = line.strip_prefix("namespace ") {
            namespace = rest.trim().to_owned();
            continue;
        }
        let word = |rest: &str| -> String {
            rest.trim().chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.').collect()
        };
        if let Some(rest) = line.strip_prefix("module ") {
            container = word(rest.trim_start_matches("rec "));
            continue;
        }
        if let Some(rest) = line.strip_prefix("type ") {
            container = word(rest);
            continue;
        }
        let mut code = line;
        while let Some(rest) = code.strip_prefix("[<") {
            let Some((attrs, after)) = rest.split_once(">]") else { break };
            let names = attrs.split(';').map(|a| a.trim().split(['(', ' ']).next().unwrap_or("").rsplit('.').next().unwrap_or(""));
            if names.into_iter().any(|a| TEST_ATTRIBUTES.contains(&a.trim_end_matches("Attribute"))) {
                pending_test = true;
            }
            code = after.trim_start();
        }
        if !pending_test || code.is_empty() {
            continue;
        }
        let decl = code.strip_prefix("let ").or_else(|| code.strip_prefix("member ").map(|m| m.split_once('.').map_or(m, |(_, n)| n)));
        let Some(decl) = decl else { continue };
        let method = match decl.trim_start().strip_prefix("``") {
            Some(quoted) => quoted.split_once("``").map(|(name, _)| name.to_owned()),
            None => Some(word(decl)).filter(|w| !w.is_empty()),
        };
        pending_test = false;
        let (Some(method), false) = (method, container.is_empty()) else { continue };
        let class = if namespace.is_empty() || container.contains('.') {
            container.clone()
        } else {
            format!("{namespace}.{container}")
        };
        let short = class.rsplit('.').next().unwrap_or(&class).to_owned();
        tests.push(test(file, stem, class.clone(), &[short.as_str()], method, ix));
    }
    tests
}

struct DotnetParser {
    jvm: JvmParser,
    /// Method names found once, for loggers that print only that.
    by_method: std::collections::HashMap<String, Option<String>>,
    results: PathBuf,
}

impl DotnetParser {
    fn new(tests: &[JvmTest], results: PathBuf) -> Self {
        let mut by_method = std::collections::HashMap::new();
        for t in tests {
            by_method
                .entry(t.method.clone())
                .and_modify(|id: &mut Option<String>| *id = None)
                .or_insert_with(|| Some(t.case.id.clone()));
        }
        Self { jvm: JvmParser::new(tests), by_method, results }
    }

    /// The id of a test as a logger names it: `Shop.Tests.CartTests.Adds`,
    /// `…Positive(n: 2)`, `…CartTests+Nested.Deep`, or only `Adds`.
    fn id(&self, display: &str) -> Option<String> {
        let name = display.split('(').next().unwrap_or(display).trim();
        if let Some((class, method)) = name.rsplit_once('.')
            && let Some(id) = self.jvm.by_class.get(&format!("{class}#{method}"))
        {
            return Some(id.clone());
        }
        self.by_method.get(name).cloned().flatten()
    }
}

impl Parser for DotnetParser {
    /// `  Passed Shop.Tests.CartTests.Adds [< 1 ms]`, with `Failed` and
    /// `Skipped`.
    fn feed(&mut self, line: &str) -> Vec<Found> {
        let Some(rest) = line.strip_prefix("  ") else { return Vec::new() };
        let (outcome, rest) = if let Some(rest) = rest.strip_prefix("Passed ") {
            (Outcome::Passed, rest)
        } else if let Some(rest) = rest.strip_prefix("Failed ") {
            (Outcome::Failed, rest)
        } else if let Some(rest) = rest.strip_prefix("Skipped ") {
            (Outcome::Skipped, rest)
        } else {
            return Vec::new();
        };
        let Some((display, _)) = rest.rsplit_once(" [") else { return Vec::new() };
        let Some(id) = self.id(display) else { return Vec::new() };
        self.jvm.record(id, outcome, Vec::new())
    }

    fn finish(&mut self) -> Vec<Found> {
        let mut found = Vec::new();
        let reports: Vec<PathBuf> = std::fs::read_dir(&self.results)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "trx"))
            .collect();
        for report in reports {
            let text = std::fs::read_to_string(&report).unwrap_or_default();
            for (class, method, outcome, lines) in parse_trx(&text) {
                if let Some(id) = self.jvm.by_class.get(&format!("{class}#{method}")).cloned() {
                    found.extend(self.jvm.record(id, outcome, lines));
                }
            }
        }
        let _ = std::fs::remove_dir_all(&self.results);
        found.extend(self.jvm.failures());
        found
    }
}

/// Each result of a TRX report: its class and method, outcome, and for a
/// failure the message and stack trace.
fn parse_trx(xml: &str) -> Vec<(String, String, Outcome, Vec<String>)> {
    let Ok(doc) = roxmltree::Document::parse(xml) else { return Vec::new() };
    let methods: std::collections::HashMap<&str, (&str, &str)> = doc
        .descendants()
        .filter(|n| n.has_tag_name("UnitTest"))
        .filter_map(|n| {
            let method = n.children().find(|c| c.has_tag_name("TestMethod"))?;
            Some((n.attribute("id")?, (method.attribute("className")?, method.attribute("name")?)))
        })
        .collect();
    doc.descendants()
        .filter(|n| n.has_tag_name("UnitTestResult"))
        .filter_map(|n| {
            let (class, method) = methods.get(n.attribute("testId")?)?;
            let outcome = match n.attribute("outcome")? {
                "Passed" => Outcome::Passed,
                "NotExecuted" | "Inconclusive" | "Pending" => Outcome::Skipped,
                _ => Outcome::Failed,
            };
            let mut lines = Vec::new();
            if outcome == Outcome::Failed {
                let text = |tag: &str| {
                    n.descendants().find(|c| c.has_tag_name(tag)).and_then(|c| c.text()).unwrap_or("").trim_end().to_owned()
                };
                lines.extend(text("Message").lines().map(str::to_owned));
                lines.extend(text("StackTrace").lines().map(str::to_owned));
            }
            // A class name may carry the generic arguments of a fixture.
            let class = class.split('(').next().unwrap_or(class);
            Some((class.to_owned(), method.to_string(), outcome, lines))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{
        CaseRef,
        test_support::{failures, parse, results},
    };

    const CS: &str = r#"namespace Shop.Tests;

public class CartTests
{
    [Fact]
    public void Adds()
    {
        Assert.Equal(2, 1 + 1);
    }

    [Fact]
    public void Fails()
    {
        Assert.Equal("{", "{");
    }

    [Fact(Skip = "later")]
    public void Later() { }

    [Theory]
    [InlineData(1)]
    [InlineData(2)]
    public void Positive(int n)
    {
        Assert.True(n > 0);
    }

    public void Helper() { }

    public class Nested
    {
        [Fact]
        public void Deep() { }
    }
}
"#;

    #[test]
    fn reads_csharp_tests() {
        let tests = parse_csharp("Shop.Tests/CartTests.cs", CS);
        let found: Vec<_> =
            tests.iter().map(|t| (t.class.as_str(), t.method.as_str(), t.case.name.as_str(), t.case.line)).collect();
        assert_eq!(
            found,
            [
                ("Shop.Tests.CartTests", "Adds", "Adds", Some(6)),
                ("Shop.Tests.CartTests", "Fails", "Fails", Some(12)),
                ("Shop.Tests.CartTests", "Later", "Later", Some(18)),
                ("Shop.Tests.CartTests", "Positive", "Positive", Some(23)),
                ("Shop.Tests.CartTests+Nested", "Deep", "Nested > Deep", Some(33)),
            ]
        );
        let nunit = "using NUnit.Framework;\nnamespace Shop.Tests\n{\n    [TestFixture]\n    public class PriceTests\n    {\n        \
            [Test, Category(\"fast\")]\n        public async Task Rounds() { }\n\n        [TestCase(1, 2)]\n        \
            [TestCase(2, 3)]\n        public void Next(int a, int b) { }\n    }\n}\n";
        let names: Vec<_> =
            parse_csharp("Tests/PriceTests.cs", nunit).into_iter().map(|t| format!("{}#{}", t.class, t.method)).collect();
        assert_eq!(names, ["Shop.Tests.PriceTests#Rounds", "Shop.Tests.PriceTests#Next"]);
    }

    #[test]
    fn reads_fsharp_tests() {
        let fs = "module Shop.Tests.Prices\n\nopen Xunit\n\n[<Fact>]\nlet ``adds tax`` () =\n    Assert.True(true)\n\n[<Theory; InlineData(1)>]\nlet rounds (n: int) = ()\n";
        let tests = parse_fsharp("Tests/Prices.fs", fs);
        let found: Vec<_> = tests.iter().map(|t| (t.class.as_str(), t.method.as_str(), t.case.line)).collect();
        assert_eq!(found, [("Shop.Tests.Prices", "adds tax", Some(6)), ("Shop.Tests.Prices", "rounds", Some(10))]);
    }

    #[test]
    fn builds_filters() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join("Shop.Tests")).unwrap();
        std::fs::write(root.join("Shop.sln"), "").unwrap();
        std::fs::write(
            root.join("Shop.Tests/Shop.Tests.csproj"),
            r#"<Project><ItemGroup><PackageReference Include="xunit" Version="2.9.2" /></ItemGroup></Project>"#,
        )
        .unwrap();
        std::fs::write(root.join("Shop.Tests/CartTests.cs"), CS).unwrap();
        assert!(Dotnet.detect(root));
        let script = |target: TestTarget| Dotnet.start(root, "dotnet", &target).unwrap().script;
        assert!(script(TestTarget::All).starts_with("dotnet test --logger 'console;verbosity=normal' --logger trx"));
        assert!(
            script(TestTarget::Paths { paths: vec!["Shop.Tests".into()] }).ends_with(
                " --filter 'FullyQualifiedName~Shop.Tests.CartTests.|FullyQualifiedName~Shop.Tests.CartTests+'"
            )
        );
        let cases =
            vec![CaseRef { id: "Shop.Tests/CartTests.cs::Positive".into(), line: Some(23) }];
        assert!(script(TestTarget::Cases { cases }).ends_with(
            " --filter 'FullyQualifiedName=Shop.Tests.CartTests.Positive|FullyQualifiedName~Shop.Tests.CartTests.Positive\\('"
        ));
        // Without a solution at the root, each test project runs.
        std::fs::remove_file(root.join("Shop.sln")).unwrap();
        assert!(script(TestTarget::All).ends_with(" 'Shop.Tests/Shop.Tests.csproj' || ok=1; exit $ok"));
    }

    #[test]
    fn reads_console_and_trx() {
        // Captured from `dotnet test` 9.0 with xUnit 2.9.
        let output = "\
[xUnit.net 00:00:00.75]     Shop.Tests.CartTests.Fails [FAIL]
  Passed Shop.Tests.CartTests+Nested.Deep [10 ms]
  Failed Shop.Tests.CartTests.Fails [41 ms]
  Error Message:
   Assert.Equal() Failure: Values differ
  Skipped Shop.Tests.CartTests.Later [1 ms]
  Passed Shop.Tests.CartTests.Positive(n: 2) [< 1 ms]
  Passed Shop.Tests.CartTests.Positive(n: 1) [< 1 ms]
  Passed Adds [< 1 ms]";
        let results_dir = report_path("dotnet-results-test");
        std::fs::create_dir_all(&results_dir).unwrap();
        std::fs::write(
            results_dir.join("run.trx"),
            r#"<?xml version="1.0" encoding="utf-8"?>
<TestRun xmlns="http://microsoft.com/schemas/VisualStudio/TeamTest/2010">
  <Results>
    <UnitTestResult testId="a" testName="Shop.Tests.CartTests.Fails" outcome="Failed">
      <Output><ErrorInfo><Message>Assert.Equal() Failure: Values differ
Expected: 3</Message><StackTrace>   at Shop.Tests.CartTests.Fails() in /p/Shop.Tests/CartTests.cs:line 14</StackTrace></ErrorInfo></Output>
    </UnitTestResult>
    <UnitTestResult testId="b" testName="Shop.Tests.CartTests.Adds" outcome="Passed" />
  </Results>
  <TestDefinitions>
    <UnitTest name="Shop.Tests.CartTests.Fails" id="a"><TestMethod className="Shop.Tests.CartTests" name="Fails" /></UnitTest>
    <UnitTest name="Shop.Tests.CartTests.Adds" id="b"><TestMethod className="Shop.Tests.CartTests" name="Adds" /></UnitTest>
  </TestDefinitions>
</TestRun>"#,
        )
        .unwrap();
        let tests = parse_csharp("Shop.Tests/CartTests.cs", CS);
        let found = parse(Box::new(DotnetParser::new(&tests, results_dir.clone())), output);
        let id = |name: &str| format!("Shop.Tests/CartTests.cs::{name}");
        let results: Vec<_> = results(&found).into_iter().map(|(id, o)| (id.to_owned(), o)).collect();
        assert_eq!(
            results,
            [
                (id("Nested > Deep"), Outcome::Passed),
                (id("Fails"), Outcome::Failed),
                (id("Later"), Outcome::Skipped),
                (id("Positive"), Outcome::Passed),
                (id("Adds"), Outcome::Passed),
            ]
        );
        let failures = failures(&found);
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].1.last().unwrap(), "   at Shop.Tests.CartTests.Fails() in /p/Shop.Tests/CartTests.cs:line 14");
        assert!(!results_dir.exists());
    }
}
