//! The Tests tab: every test of every repository, run all together, by
//! folder, by file or one at a time, with the runner's output in the
//! center panel and the outcome in the status bar.

use std::{
    any::Any,
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    ops::Range,
    rc::Rc,
};

use difftower_core::{CaseRef, Outcome, Runner, TestEvent, TestSuite, TestTarget};
use serde::{Deserialize, Serialize};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::{SidebarTab, Workspace};
use crate::file_icons;

const ROW_HEIGHT: Pixels = px(22.);
const NAME_SIZE: Pixels = px(13.);
const DETAIL_SIZE: Pixels = px(12.);
const INDENT: f32 = 8.;
/// Output beyond this many lines drops its oldest ones.
const OUTPUT_LIMIT: usize = 50_000;

/// Where a test stands after the last run that covered it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum Status {
    Running,
    Passed,
    Failed,
    Skipped,
}

/// One runner invocation, waiting or in progress.
struct Job {
    repo: String,
    runner: Runner,
    target: TestTarget,
    /// The tests it covers, shown as running until it reports them.
    ids: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RowKind {
    Suite,
    Dir,
    File,
    /// A test, by index in its suite.
    Case(usize),
}

struct Row {
    suite: usize,
    kind: RowKind,
    /// The directory or file, as a workspace path; empty for a suite.
    path: String,
    label: String,
    depth: usize,
    expanded: bool,
}

#[derive(Clone, Copy)]
enum AfterListing {
    All,
    Failed,
    Changed,
}

pub(super) struct Tests {
    suites: Vec<TestSuite>,
    /// The suites were listed at least once.
    listed: bool,
    listing: bool,
    /// What to run once the listing in progress is done.
    after_listing: Option<AfterListing>,
    _list_task: Option<Task<()>>,
    status: HashMap<String, Status>,
    /// What the runner said about each failed test.
    details: HashMap<String, Vec<SharedString>>,
    /// The failed test whose details the output shows instead of the log.
    focus: Option<String>,
    /// Where results are kept between sessions.
    store: Option<std::path::PathBuf>,
    output: Vec<SharedString>,
    queue: VecDeque<Job>,
    current: Option<Job>,
    /// Dropping it stops the run in progress.
    handle: Option<Box<dyn Any + Send>>,
    _run_task: Option<Task<()>>,
    /// The center panel shows the output instead of the diff.
    pub show_output: bool,
    /// Suites folded in the tree, by index.
    collapsed: HashSet<usize>,
    /// Directories and files unfolded in the tree, as `suite:path`.
    expanded: HashSet<String>,
    rows: Rc<Vec<Row>>,
    output_scroll: UniformListScrollHandle,
    /// Changes with every change to the output, for the center panel.
    pub version: u64,
}

impl Default for Tests {
    fn default() -> Self {
        Self {
            suites: Vec::new(),
            listed: false,
            listing: false,
            after_listing: None,
            _list_task: None,
            status: HashMap::new(),
            details: HashMap::new(),
            focus: None,
            store: None,
            output: Vec::new(),
            queue: VecDeque::new(),
            current: None,
            handle: None,
            _run_task: None,
            show_output: false,
            collapsed: HashSet::new(),
            expanded: HashSet::new(),
            rows: Rc::default(),
            output_scroll: UniformListScrollHandle::new(),
            version: 0,
        }
    }
}

/// Results as kept between sessions.
#[derive(Default, Serialize, Deserialize)]
struct Stored {
    status: HashMap<String, Status>,
    details: HashMap<String, Vec<String>>,
}

impl Tests {
    /// Tests with the results of the last session on the same repository,
    /// `key` naming it, such as `host:/path`.
    pub fn load(key: &str) -> Self {
        let store = results_dir().map(|dir| dir.join(format!("{:016x}.json", fnv(key.as_bytes()))));
        let stored: Stored = store
            .as_ref()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            status: stored.status,
            details: stored
                .details
                .into_iter()
                .map(|(id, lines)| (id, lines.into_iter().map(SharedString::from).collect()))
                .collect(),
            store,
            ..Self::default()
        }
    }

    fn save(&self) {
        let Some(path) = &self.store else { return };
        let stored = Stored {
            status: self.status.iter().filter(|(_, s)| **s != Status::Running).map(|(id, s)| (id.clone(), *s)).collect(),
            details: self
                .details
                .iter()
                .map(|(id, lines)| (id.clone(), lines.iter().map(|l| l.to_string()).collect()))
                .collect(),
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_vec(&stored) {
            let _ = std::fs::write(path, json);
        }
    }

    fn in_current_job(&self, id: &str) -> bool {
        self.current.as_ref().is_some_and(|job| job.ids.iter().any(|i| i == id))
    }

    pub fn running(&self) -> bool {
        self.current.is_some() || !self.queue.is_empty()
    }

    fn count(&self, status: Status) -> usize {
        self.status.values().filter(|s| **s == status).count()
    }

    pub fn failed(&self) -> usize {
        self.count(Status::Failed)
    }

    /// The tests of `suite` at or under `path`, or all of them for none.
    fn ids_under<'a>(&'a self, suite: usize, path: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.suites[suite]
            .tests
            .iter()
            .filter(move |t| {
                path.is_empty()
                    || t.file == path
                    || t.file.strip_prefix(path).is_some_and(|rest| rest.starts_with('/'))
            })
            .map(|t| t.id.as_str())
    }

    /// The worst status among `ids`: failed, then running, then passed.
    fn summary<'a>(&self, ids: impl Iterator<Item = &'a str>) -> Option<Status> {
        let (mut any, mut passed, mut skipped, mut total) = (None, 0, 0, 0);
        for id in ids {
            total += 1;
            match self.status.get(id) {
                Some(Status::Failed) => return Some(Status::Failed),
                Some(Status::Running) => any = Some(Status::Running),
                Some(Status::Passed) => passed += 1,
                Some(Status::Skipped) => skipped += 1,
                None => {}
            }
        }
        any.or((total > 0 && passed + skipped == total).then_some(if passed > 0 {
            Status::Passed
        } else {
            Status::Skipped
        }))
    }

    fn rebuild_rows(&mut self) {
        #[derive(Default)]
        struct Node {
            dirs: BTreeMap<String, Node>,
            files: BTreeMap<String, Vec<usize>>,
        }
        fn emit(tests: &Tests, suite: usize, node: &Node, base: &str, depth: usize, rows: &mut Vec<Row>) {
            for (name, mut child) in &node.dirs {
                // Folders holding only one folder show as one row.
                let mut label = name.clone();
                while child.files.is_empty() && child.dirs.len() == 1 {
                    let (next, grandchild) = child.dirs.iter().next().expect("one");
                    label = format!("{label}/{next}");
                    child = grandchild;
                }
                let path = format!("{base}{label}");
                let expanded = tests.expanded.contains(&format!("{suite}:{path}"));
                rows.push(Row { suite, kind: RowKind::Dir, path: path.clone(), label, depth, expanded });
                if expanded {
                    emit(tests, suite, child, &format!("{path}/"), depth + 1, rows);
                }
            }
            for (name, cases) in &node.files {
                let path = format!("{base}{name}");
                let expanded = tests.expanded.contains(&format!("{suite}:{path}"));
                rows.push(Row { suite, kind: RowKind::File, path: path.clone(), label: name.clone(), depth, expanded });
                if expanded {
                    for &ix in cases {
                        let case = &tests.suites[suite].tests[ix];
                        rows.push(Row {
                            suite,
                            kind: RowKind::Case(ix),
                            path: case.file.clone(),
                            label: case.name.clone(),
                            depth: depth + 1,
                            expanded: false,
                        });
                    }
                }
            }
        }

        let mut rows = Vec::new();
        for (ix, suite) in self.suites.iter().enumerate() {
            let expanded = !self.collapsed.contains(&ix);
            rows.push(Row { suite: ix, kind: RowKind::Suite, path: String::new(), label: String::new(), depth: 0, expanded });
            if !expanded {
                continue;
            }
            let prefix = if suite.repo.is_empty() { String::new() } else { format!("{}/", suite.repo) };
            let mut root = Node::default();
            for (case_ix, case) in suite.tests.iter().enumerate() {
                let relative = case.file.strip_prefix(&prefix).unwrap_or(&case.file);
                let mut node = &mut root;
                let mut parts: Vec<&str> = relative.split('/').collect();
                let file = parts.pop().unwrap_or_default();
                for dir in parts {
                    node = node.dirs.entry(dir.to_owned()).or_default();
                }
                node.files.entry(file.to_owned()).or_default().push(case_ix);
            }
            emit(self, ix, &root, &prefix, 1, &mut rows);
        }
        self.rows = Rc::new(rows);
    }

    fn push_output(&mut self, line: impl Into<SharedString>) {
        self.output.push(line.into());
        if self.output.len() > OUTPUT_LIMIT {
            self.output.drain(..OUTPUT_LIMIT / 5);
        }
    }
}

impl Workspace {
    /// Lists every repository's tests in the background.
    pub(super) fn list_tests(&mut self, cx: &mut Context<Self>) {
        if self.tests.listing {
            return;
        }
        self.tests.listing = true;
        let backend = self.backend.clone();
        self.tests._list_task = Some(cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move { backend.tests() }).await;
            this.update(cx, |this, cx| {
                let tests = &mut this.tests;
                tests.listing = false;
                tests.listed = true;
                match result {
                    Ok(suites) => {
                        // Folds keep to the same suites if the list changed.
                        if suites.len() != tests.suites.len() {
                            tests.collapsed.clear();
                        }
                        tests.suites = suites;
                        tests.rebuild_rows();
                    }
                    Err(err) => this.error = Some(format!("Could not list the tests: {err:#}").into()),
                }
                match this.tests.after_listing.take() {
                    Some(AfterListing::All) => this.run_all_tests(cx),
                    Some(AfterListing::Failed) => this.rerun_failed_tests(cx),
                    Some(AfterListing::Changed) => this.run_changed_tests(cx),
                    None => {}
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Lists the tests the first time the tab shows.
    pub(super) fn ensure_tests_listed(&mut self, cx: &mut Context<Self>) {
        if !self.tests.listed {
            self.list_tests(cx);
        }
    }

    pub(super) fn run_all_tests(&mut self, cx: &mut Context<Self>) {
        if !self.tests.listed {
            self.tests.after_listing = Some(AfterListing::All);
            self.list_tests(cx);
            return;
        }
        let jobs = (0..self.tests.suites.len()).map(|ix| self.job(ix, RowKind::Suite, "")).collect();
        self.run_jobs(jobs, cx);
    }

    fn job(&self, suite: usize, kind: RowKind, path: &str) -> Job {
        let tests = &self.tests;
        let s = &tests.suites[suite];
        let (target, ids) = match kind {
            RowKind::Suite => (TestTarget::All, tests.ids_under(suite, "").map(str::to_owned).collect()),
            RowKind::Dir | RowKind::File => (
                TestTarget::Paths { paths: vec![path.to_owned()] },
                tests.ids_under(suite, path).map(str::to_owned).collect(),
            ),
            RowKind::Case(ix) => {
                let case = &s.tests[ix];
                (TestTarget::Cases { cases: vec![CaseRef { id: case.id.clone(), line: case.line }] }, vec![case.id.clone()])
            }
        };
        Job { repo: s.repo.clone(), runner: s.runner, target, ids }
    }

    /// One job per suite running `ids` among its tests.
    fn case_jobs(&self, ids: &HashSet<&str>) -> Vec<Job> {
        self.tests
            .suites
            .iter()
            .filter_map(|suite| {
                let cases: Vec<CaseRef> = suite
                    .tests
                    .iter()
                    .filter(|t| ids.contains(t.id.as_str()))
                    .map(|t| CaseRef { id: t.id.clone(), line: t.line })
                    .collect();
                (!cases.is_empty()).then(|| Job {
                    repo: suite.repo.clone(),
                    runner: suite.runner,
                    ids: cases.iter().map(|c| c.id.clone()).collect(),
                    target: TestTarget::Cases { cases },
                })
            })
            .collect()
    }

    pub(super) fn rerun_failed_tests(&mut self, cx: &mut Context<Self>) {
        if !self.tests.listed {
            self.tests.after_listing = Some(AfterListing::Failed);
            self.list_tests(cx);
            return;
        }
        let failed: HashSet<&str> =
            self.tests.status.iter().filter(|(_, s)| **s == Status::Failed).map(|(id, _)| id.as_str()).collect();
        let jobs = self.case_jobs(&failed);
        if !jobs.is_empty() {
            self.run_jobs(jobs, cx);
        }
    }

    /// Runs the test files among the changes, and the tests named after a
    /// changed file: `user.rb` brings `user_test.rb`, `test_user.py` or
    /// `user.test.ts`.
    pub(super) fn run_changed_tests(&mut self, cx: &mut Context<Self>) {
        if !self.tests.listed {
            self.tests.after_listing = Some(AfterListing::Changed);
            self.list_tests(cx);
            return;
        }
        let changed: Vec<&str> = self.changes.iter().map(|c| c.path.as_str()).collect();
        let stems: HashSet<String> = changed.iter().map(|path| test_stem(path)).collect();
        let jobs: Vec<Job> = self
            .tests
            .suites
            .iter()
            .filter_map(|suite| {
                let mut files: Vec<String> = suite
                    .tests
                    .iter()
                    .map(|t| t.file.as_str())
                    .filter(|file| changed.contains(file) || stems.contains(&test_stem(file)))
                    .map(str::to_owned)
                    .collect();
                files.dedup();
                (!files.is_empty()).then(|| Job {
                    repo: suite.repo.clone(),
                    runner: suite.runner,
                    ids: suite.tests.iter().filter(|t| files.contains(&t.file)).map(|t| t.id.clone()).collect(),
                    target: TestTarget::Paths { paths: files },
                })
            })
            .collect();
        if jobs.is_empty() {
            self.error = Some("No tests match the changed files".into());
            cx.notify();
            return;
        }
        self.run_jobs(jobs, cx);
    }

    /// Stops whatever runs and runs `jobs` one after another.
    fn run_jobs(&mut self, jobs: Vec<Job>, cx: &mut Context<Self>) {
        self.stop_tests(cx);
        let tests = &mut self.tests;
        tests.queue = jobs.into();
        for job in &tests.queue {
            for id in &job.ids {
                tests.status.insert(id.clone(), Status::Running);
                tests.details.remove(id);
            }
        }
        tests.output.clear();
        tests.focus = None;
        tests.show_output = true;
        self.start_next_job(cx);
    }

    pub(super) fn stop_tests(&mut self, cx: &mut Context<Self>) {
        let tests = &mut self.tests;
        let waiting: Vec<Job> = tests.queue.drain(..).collect();
        for id in waiting.iter().flat_map(|job| &job.ids) {
            if tests.status.get(id) == Some(&Status::Running) {
                tests.status.remove(id);
            }
        }
        // Dropping the handle stops the runner; it still reports finishing.
        if tests.handle.take().is_some() {
            tests.push_output("Stopped.");
        }
        cx.notify();
    }

    fn start_next_job(&mut self, cx: &mut Context<Self>) {
        let Some(job) = self.tests.queue.pop_front() else {
            let was_running = self.tests._run_task.take().is_some();
            if was_running {
                self.tests.save();
                let failed = self.tests.failed();
                if failed > 0 {
                    self.notify_user(&format!("{failed} test{} failed", if failed == 1 { "" } else { "s" }), "");
                }
                // New or renamed tests show after a run.
                self.list_tests(cx);
            }
            return;
        };
        let repo_label = if job.repo.is_empty() { self.backend.name() } else { job.repo.clone() };
        let what = match &job.target {
            TestTarget::All => "all tests".to_owned(),
            TestTarget::Paths { paths } => paths.join(", "),
            TestTarget::Cases { cases } if cases.len() == 1 => cases[0].id.clone(),
            TestTarget::Cases { cases } => format!("{} tests", cases.len()),
        };
        self.tests.push_output(format!("▶ {repo_label} · {}: {what}", job.runner.label()));
        let (repo, runner, target) = (job.repo.clone(), job.runner, job.target.clone());
        self.tests.current = Some(job);
        self.tests.version += 1;

        let backend = self.backend.clone();
        let (tx, rx) = async_channel::unbounded::<TestEvent>();
        self.tests._run_task = Some(cx.spawn(async move |this, cx| {
            let started = cx
                .background_executor()
                .spawn(async move {
                    backend.run_tests(&repo, runner, target, Box::new(move |event| {
                        let _ = tx.try_send(event);
                    }))
                })
                .await;
            let stopped = this
                .update(cx, |this, cx| match started {
                    Ok(handle) => {
                        this.tests.handle = Some(handle);
                        false
                    }
                    Err(err) => {
                        this.apply_test_events(
                            vec![TestEvent::Output { line: format!("Could not run: {err:#}") }, TestEvent::Finished { success: false }],
                            cx,
                        );
                        true
                    }
                })
                .unwrap_or(true);
            if stopped {
                return;
            }
            while let Ok(event) = rx.recv().await {
                let mut events = vec![event];
                while let Ok(event) = rx.try_recv() {
                    events.push(event);
                }
                let finished = matches!(events.last(), Some(TestEvent::Finished { .. }));
                if this.update(cx, |this, cx| this.apply_test_events(events, cx)).is_err() || finished {
                    break;
                }
            }
        }));
        cx.notify();
    }

    fn apply_test_events(&mut self, events: Vec<TestEvent>, cx: &mut Context<Self>) {
        let mut finished = false;
        for event in events {
            match event {
                TestEvent::Output { line } => self.tests.push_output(line),
                // Runners report more than they were asked for, such as the
                // rest of a file as skipped; those keep their last result.
                TestEvent::Result { id, .. } | TestEvent::Failure { id, .. } if !self.tests.in_current_job(&id) => {}
                TestEvent::Result { id, outcome } => {
                    let status = match outcome {
                        Outcome::Passed => Status::Passed,
                        Outcome::Failed => Status::Failed,
                        Outcome::Skipped => Status::Skipped,
                    };
                    self.tests.status.insert(id, status);
                }
                TestEvent::Failure { id, lines } => {
                    self.tests.details.insert(id, lines.into_iter().map(SharedString::from).collect());
                }
                TestEvent::Finished { success } => {
                    finished = true;
                    let tests = &mut self.tests;
                    if let Some(job) = tests.current.take() {
                        // Tests it never reported, as when it could not start.
                        for id in &job.ids {
                            if tests.status.get(id) == Some(&Status::Running) {
                                tests.status.remove(id);
                            }
                        }
                    }
                    tests.handle = None;
                    tests.push_output(if success { "✓ Done" } else { "✗ Finished with failures" });
                    tests.push_output("");
                }
            }
        }
        self.tests.version += 1;
        self.tests.output_scroll.scroll_to_bottom();
        if finished {
            self.start_next_job(cx);
        }
        cx.notify();
    }

    fn run_row(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(row) = self.tests.rows.get(ix) else { return };
        let job = self.job(row.suite, row.kind, &row.path);
        self.run_jobs(vec![job], cx);
    }

    fn toggle_test_row(&mut self, ix: usize, cx: &mut Context<Self>) {
        let rows = self.tests.rows.clone();
        let Some(row) = rows.get(ix) else { return };
        let tests = &mut self.tests;
        match row.kind {
            RowKind::Suite => {
                if !tests.collapsed.remove(&row.suite) {
                    tests.collapsed.insert(row.suite);
                }
            }
            RowKind::Dir | RowKind::File => {
                let key = format!("{}:{}", row.suite, row.path);
                if !tests.expanded.remove(&key) {
                    tests.expanded.insert(key);
                }
            }
            RowKind::Case(_) => return,
        }
        tests.rebuild_rows();
        cx.notify();
    }

    pub(super) fn show_test_output(&mut self, cx: &mut Context<Self>) {
        self.tests.show_output = true;
        self.tab = SidebarTab::Tests;
        self.ensure_tests_listed(cx);
        cx.notify();
    }

    pub(super) fn render_tests(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let tests = &self.tests;
        let running = tests.running();
        let header = h_flex()
            .h(ROW_HEIGHT)
            .pl_3()
            .pr_1()
            .gap_1()
            .text_size(DETAIL_SIZE)
            .text_color(theme.muted_foreground)
            .child(div().flex_1().min_w_0().truncate().child(if tests.listing {
                "Looking for tests…".to_owned()
            } else {
                let total: usize = tests.suites.iter().map(|s| s.tests.len()).sum();
                format!("{total} test{}", if total == 1 { "" } else { "s" })
            }))
            .when(!running && tests.failed() > 0, |el| {
                el.child(
                    Button::new("tests-rerun-failed")
                        .xsmall()
                        .ghost()
                        .icon(Icon::new(Lucide::RotateCcw).text_color(theme.danger))
                        .tooltip("Run the failed tests again")
                        .on_click(cx.listener(|this, _, _, cx| this.rerun_failed_tests(cx))),
                )
            })
            .when(!running, |el| {
                el.child(
                    Button::new("tests-changed")
                        .xsmall()
                        .ghost()
                        .icon(Icon::new(Lucide::FileDiff))
                        .tooltip("Run the tests of the changed files")
                        .on_click(cx.listener(|this, _, _, cx| this.run_changed_tests(cx))),
                )
            })
            .child(
                Button::new("tests-output")
                    .xsmall()
                    .ghost()
                    .icon(Icon::new(Lucide::ScrollText))
                    .tooltip("Show the output")
                    .on_click(cx.listener(|this, _, _, cx| this.show_test_output(cx))),
            )
            .child(
                Button::new("tests-refresh")
                    .xsmall()
                    .ghost()
                    .icon(Icon::new(Lucide::RefreshCw))
                    .tooltip("Look for tests again")
                    .on_click(cx.listener(|this, _, _, cx| this.list_tests(cx))),
            )
            .child(if running {
                Button::new("tests-stop")
                    .xsmall()
                    .ghost()
                    .icon(Icon::new(Lucide::CircleStop).text_color(theme.danger))
                    .tooltip("Stop")
                    .on_click(cx.listener(|this, _, _, cx| this.stop_tests(cx)))
            } else {
                Button::new("tests-run-all")
                    .xsmall()
                    .ghost()
                    .icon(Icon::new(Lucide::Play).text_color(theme.success))
                    .tooltip("Run every test")
                    .on_click(cx.listener(|this, _, _, cx| this.run_all_tests(cx)))
            });

        let body = if tests.listed && tests.suites.is_empty() && !tests.listing {
            v_flex()
                .size_full()
                .p_4()
                .gap_2()
                .items_center()
                .justify_center()
                .text_sm()
                .text_center()
                .text_color(theme.muted_foreground)
                .child(Icon::new(Lucide::FlaskConical).large())
                .child("No tests found.")
                .child("Difftower runs pytest, vitest and Rails tests. Set the runner and its command under [tests] in .difftower.toml.")
                .into_any_element()
        } else {
            uniform_list(
                "tests",
                tests.rows.len(),
                cx.processor(|this, range: Range<usize>, _, cx| {
                    range.map(|ix| this.render_test_row(ix, cx)).collect::<Vec<_>>()
                }),
            )
            .size_full()
            .py_1()
            .into_any_element()
        };
        v_flex().size_full().child(header).child(div().flex_1().min_h_0().child(body))
    }

    fn render_test_row(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let tests = &self.tests;
        let row = &tests.rows[ix];
        let suite = &tests.suites[row.suite];
        let status = match row.kind {
            RowKind::Case(case) => tests.status.get(&suite.tests[case].id).copied(),
            _ => tests.summary(tests.ids_under(row.suite, &row.path)),
        };
        let (status_icon, status_color) = match status {
            Some(Status::Passed) => (Lucide::CircleCheck, theme.success),
            Some(Status::Failed) => (Lucide::CircleX, theme.danger),
            Some(Status::Running) => (Lucide::LoaderCircle, theme.blue),
            Some(Status::Skipped) => (Lucide::CircleMinus, theme.muted_foreground),
            None => (Lucide::Circle, theme.muted_foreground.opacity(0.5)),
        };
        let chevron = |expanded: bool| {
            Icon::new(if expanded { IconName::ChevronDown } else { IconName::ChevronRight })
                .small()
                .text_color(theme.muted_foreground)
        };

        let mut el = h_flex()
            .id(("test-row", ix))
            .group("test-row")
            .w_full()
            .h(ROW_HEIGHT)
            .pl(px(4. + row.depth as f32 * INDENT))
            .pr_1()
            .gap_1()
            .text_size(NAME_SIZE)
            .cursor_pointer()
            .hover(|el| el.bg(theme.list_hover));
        el = match row.kind {
            RowKind::Suite => {
                let name = if suite.repo.is_empty() { self.backend.name() } else { suite.repo.clone() };
                el.child(div().w_4().flex_none().child(chevron(row.expanded)))
                    .child(Icon::new(status_icon).small().flex_none().text_color(status_color))
                    .child(div().flex_none().font_semibold().child(name))
                    .child(div().text_size(DETAIL_SIZE).text_color(theme.muted_foreground).child(suite.runner.label()))
                    .child(div().flex_1().min_w_0().truncate().text_size(DETAIL_SIZE).when_some(
                        suite.error.as_ref(),
                        |el, err| el.text_color(theme.danger).child(err.lines().last().unwrap_or_default().to_owned()),
                    ))
                    .when(suite.error.is_none(), |el| {
                        el.child(
                            div().text_size(DETAIL_SIZE).text_color(theme.muted_foreground).child(suite.tests.len().to_string()),
                        )
                    })
            }
            RowKind::Dir => el
                .child(div().w_4().flex_none().child(chevron(row.expanded)))
                .child(Icon::new(status_icon).small().flex_none().text_color(status_color))
                .child(
                    Icon::new(if row.expanded { IconName::FolderOpen } else { IconName::FolderClosed })
                        .small()
                        .flex_none()
                        .text_color(theme.muted_foreground),
                )
                .child(div().flex_1().min_w_0().truncate().child(row.label.clone())),
            RowKind::File => el
                .child(div().w_4().flex_none().child(chevron(row.expanded)))
                .child(Icon::new(status_icon).small().flex_none().text_color(status_color))
                .child(file_icons::icon(&row.label))
                .child(div().flex_1().min_w_0().truncate().child(row.label.clone())),
            RowKind::Case(_) => el
                .child(div().w_4().flex_none())
                .child(Icon::new(status_icon).small().flex_none().text_color(status_color))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .when(status == Some(Status::Failed), |el| el.text_color(theme.danger))
                        .child(row.label.clone()),
                ),
        };
        let can_run = suite.error.is_none() || row.kind != RowKind::Suite;
        let error = suite.error.clone();
        el.when(can_run, |el| {
            el.child(
                div().flex_none().opacity(0.).group_hover("test-row", |s| s.opacity(1.)).child(
                    Button::new(("run-test", ix))
                        .xsmall()
                        .ghost()
                        .icon(Icon::new(Lucide::Play).text_color(theme.success))
                        .tooltip("Run")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.run_row(ix, cx);
                        })),
                ),
            )
        })
        .on_click(cx.listener(move |this, _, window, cx| {
            let Some(row) = this.tests.rows.get(ix) else { return };
            match row.kind {
                RowKind::Case(case) => {
                    let test = &this.tests.suites[row.suite].tests[case];
                    let (id, path, line) = (test.id.clone(), row.path.clone(), test.line);
                    if this.tests.details.contains_key(&id) {
                        this.tests.focus = Some(id);
                        this.tests.version += 1;
                        this.tests.show_output = true;
                        cx.notify();
                    } else {
                        this.open_at(&path, line, window, cx);
                    }
                }
                RowKind::Suite if error.is_some() => {
                    let error = error.clone().unwrap_or_default();
                    for line in error.lines() {
                        this.tests.push_output(line.to_owned());
                    }
                    this.tests.version += 1;
                    this.show_test_output(cx);
                }
                _ => this.toggle_test_row(ix, cx),
            }
        }))
        .into_any_element()
    }

    /// The runner's output in the center panel, or what it said about one
    /// failed test.
    pub(super) fn render_test_output(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let tests = &self.tests;
        let running = tests.running();
        let (passed, failed) = (tests.count(Status::Passed), tests.failed());
        let focused = tests.focus.as_ref().and_then(|id| {
            let case = tests.suites.iter().flat_map(|s| &s.tests).find(|t| &t.id == id);
            Some((id.clone(), tests.details.get(id)?.clone(), case.cloned()))
        });
        let lines: Rc<Vec<SharedString>> = Rc::new(match &focused {
            Some((_, lines, _)) => lines.clone(),
            None => tests.output.clone(),
        });
        let title: SharedString = match &focused {
            Some((id, _, case)) => case.as_ref().map_or(id.clone(), |c| format!("{} · {}", c.file, c.name)).into(),
            None if running => "Running tests…".into(),
            None => "Test output".into(),
        };
        let header = h_flex()
            .h_8()
            .px_3()
            .gap_2()
            .text_sm()
            .border_b_1()
            .border_color(theme.border)
            .child(
                Icon::new(if focused.is_some() { Lucide::CircleX } else { Lucide::FlaskConical })
                    .xsmall()
                    .text_color(if focused.is_some() { theme.danger } else { theme.muted_foreground }),
            )
            .child(div().flex_1().min_w_0().truncate().child(title));
        let header = match focused {
            Some((_, _, case)) => header
                .when_some(case, |el, case| {
                    let (path, line) = (case.file.clone(), case.line);
                    el.child(
                        Button::new("open-test")
                            .xsmall()
                            .ghost()
                            .icon(IconName::File)
                            .label("Open")
                            .tooltip("Open the test in its file")
                            .on_click(cx.listener(move |this, _, window, cx| this.open_at(&path, line, window, cx))),
                    )
                })
                .child(
                    Button::new("all-output")
                        .xsmall()
                        .ghost()
                        .icon(Icon::new(Lucide::ScrollText))
                        .label("All output")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.tests.focus = None;
                            this.tests.version += 1;
                            cx.notify();
                        })),
                ),
            None => header
                .child(div().text_xs().text_color(theme.success).child(format!("{passed} passed")))
                .when(failed > 0, |el| el.child(div().text_xs().text_color(theme.danger).child(format!("{failed} failed")))),
        }
        .child(
            Button::new("close-output")
                .xsmall()
                .ghost()
                .icon(IconName::ArrowLeft)
                .label("Changes")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.tests.show_output = false;
                    cx.notify();
                })),
        );
        let list = uniform_list("test-output", lines.len(), {
            let lines = lines.clone();
            move |range: Range<usize>, _, cx: &mut App| {
                let theme = cx.theme();
                range
                    .map(|ix| {
                        let line = lines[ix].clone();
                        div().h(px(18.)).px_3().whitespace_nowrap().text_color(line_color(&line, theme)).child(line).into_any_element()
                    })
                    .collect::<Vec<_>>()
            }
        })
        .size_full()
        .py_1();
        let list = if tests.focus.is_none() { list.track_scroll(&tests.output_scroll) } else { list };
        v_flex()
            .size_full()
            .child(header)
            .child(div().flex_1().min_h_0().font_family(crate::terminal::monospace_family()).text_size(px(12.)).child(list))
            .into_any_element()
    }

    /// The status bar's test summary: what failed, or what runs.
    pub(super) fn render_test_status(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = cx.theme();
        let tests = &self.tests;
        let (passed, failed) = (tests.count(Status::Passed), tests.failed());
        let (icon, color, label) = if tests.running() {
            let label = match failed {
                0 => format!("Running tests · {passed} passed"),
                n => format!("Running tests · {n} failed"),
            };
            (Lucide::LoaderCircle, theme.blue, label)
        } else if failed > 0 {
            (Lucide::CircleX, theme.danger, format!("{failed} test{} failed", if failed == 1 { "" } else { "s" }))
        } else if passed > 0 {
            (Lucide::CircleCheck, theme.success, format!("{passed} test{} passed", if passed == 1 { "" } else { "s" }))
        } else {
            return None;
        };
        let rerun = failed > 0 && !tests.running();
        Some(
            h_flex()
                .gap_2()
                .child(
                    h_flex()
                        .id("test-status")
                        .gap_1p5()
                        .cursor_pointer()
                        .text_color(color)
                        .when(rerun, |el| el.font_semibold())
                        .child(Icon::new(icon).xsmall())
                        .child(label)
                        .on_click(cx.listener(|this, _, _, cx| this.show_test_output(cx))),
                )
                .when(rerun, |el| {
                    el.child(
                        Button::new("rerun-failed")
                            .xsmall()
                            .ghost()
                            .icon(Icon::new(Lucide::RotateCcw))
                            .label("Re-run failed")
                            .on_click(cx.listener(|this, _, _, cx| this.rerun_failed_tests(cx))),
                    )
                })
                .into_any_element(),
        )
    }
}

/// `$XDG_STATE_HOME/difftower/tests`, defaulting to `~/.local/state`.
fn results_dir() -> Option<std::path::PathBuf> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| std::path::Path::new(&home).join(".local/state")))?;
    Some(state.join("difftower/tests"))
}

/// FNV-1a, stable across builds unlike `DefaultHasher`.
fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3))
}

/// A file's name without its directory, extension and test markers, so a
/// source file and its tests share it: `app/models/user.rb`,
/// `test/models/user_test.rb`, `tests/test_user.py` and `user.test.ts` all
/// give `user`.
fn test_stem(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    let mut stem = name.split('.').next().unwrap_or(name);
    for suffix in ["_test", "_spec"] {
        stem = stem.strip_suffix(suffix).unwrap_or(stem);
    }
    stem.strip_prefix("test_").unwrap_or(stem).to_owned()
}

/// Failures in red and passes in green, as the runners print them.
fn line_color(line: &str, theme: &gpui_kit::component::Theme) -> Hsla {
    let trimmed = line.trim_start();
    let failed = trimmed.starts_with('×')
        || trimmed.starts_with("FAILED")
        || trimmed.starts_with("ERROR")
        || trimmed.starts_with("E ")
        || trimmed.starts_with('✗')
        || line.contains(" FAILED")
        || line.ends_with(" = F")
        || line.ends_with(" = E");
    let passed = trimmed.starts_with('✓') || line.contains(" PASSED") || line.ends_with(" = .");
    if failed {
        theme.danger
    } else if passed {
        theme.success
    } else if trimmed.starts_with('▶') {
        theme.foreground
    } else {
        theme.muted_foreground
    }
}

#[cfg(test)]
mod tests {
    use difftower_core::{Runner, TestCase, TestSuite};

    // Not a glob: gpui's `test` attribute would shadow the standard one.
    use super::{Status, Tests, test_stem};

    fn case(file: &str, name: &str) -> TestCase {
        TestCase { id: format!("{file}::{name}"), file: file.into(), name: name.into(), line: None }
    }

    fn panel() -> Tests {
        let mut tests = Tests::default();
        tests.suites = vec![TestSuite {
            repo: "web".into(),
            runner: Runner::Vitest,
            tests: vec![
                case("web/src/lib/calc/a.test.ts", "adds"),
                case("web/src/lib/calc/a.test.ts", "subtracts"),
                case("web/src/lib/calc/b.test.ts", "divides"),
                case("web/tests/urls.test.ts", "lists"),
            ],
            error: None,
        }];
        tests.rebuild_rows();
        tests
    }

    fn labels(tests: &Tests) -> Vec<(usize, &str)> {
        tests.rows.iter().map(|row| (row.depth, row.label.as_str())).collect()
    }

    #[test]
    fn folders_with_one_folder_inside_show_as_one_row() {
        let mut tests = panel();
        assert_eq!(labels(&tests), [(0, ""), (1, "src/lib/calc"), (1, "tests")]);
        assert_eq!(tests.rows[1].path, "web/src/lib/calc");

        tests.expanded.insert("0:web/src/lib/calc".into());
        tests.expanded.insert("0:web/src/lib/calc/a.test.ts".into());
        tests.rebuild_rows();
        assert_eq!(
            labels(&tests),
            [(0, ""), (1, "src/lib/calc"), (2, "a.test.ts"), (3, "adds"), (3, "subtracts"), (2, "b.test.ts"), (1, "tests")]
        );

        tests.collapsed.insert(0);
        tests.rebuild_rows();
        assert_eq!(labels(&tests), [(0, "")]);
    }

    #[test]
    fn a_folder_shows_its_worst_test() {
        let mut tests = panel();
        let calc = |tests: &Tests| tests.summary(tests.ids_under(0, "web/src/lib/calc"));
        assert_eq!(calc(&tests), None);
        for id in ["web/src/lib/calc/a.test.ts::adds", "web/src/lib/calc/a.test.ts::subtracts"] {
            tests.status.insert(id.into(), Status::Passed);
        }
        // b.test.ts has not run yet.
        assert_eq!(calc(&tests), None);
        tests.status.insert("web/src/lib/calc/b.test.ts::divides".into(), Status::Running);
        assert_eq!(calc(&tests), Some(Status::Running));
        tests.status.insert("web/src/lib/calc/b.test.ts::divides".into(), Status::Passed);
        assert_eq!(calc(&tests), Some(Status::Passed));
        tests.status.insert("web/src/lib/calc/a.test.ts::adds".into(), Status::Failed);
        assert_eq!(calc(&tests), Some(Status::Failed));
        // `web/src/lib/calc` is not a prefix of a sibling like `calcx`.
        assert_eq!(tests.ids_under(0, "web/src/lib/cal").count(), 0);
    }

    #[test]
    fn a_source_file_and_its_tests_share_a_stem() {
        for path in ["app/models/user.rb", "test/models/user_test.rb", "tests/test_user.py", "src/user.test.ts", "user_spec.rb"] {
            assert_eq!(test_stem(path), "user", "{path}");
        }
        assert_ne!(test_stem("app/models/users.rb"), "user");
    }
}
