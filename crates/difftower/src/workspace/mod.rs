mod diff_view;
mod finder;
mod main_panel;
mod session;
mod sidebar;
mod test_panel;

use std::{
    any::Any,
    collections::{HashMap, HashSet},
    hash::{DefaultHasher, Hash, Hasher},
    rc::Rc,
    sync::Arc,
};

use difftower_core::{Backend, Change, Guard, Status};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Selectable as _, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    diff::{DiffFile, DiffLinePosition, DiffMode, DiffSide, DiffState},
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::{
    file_tree::{self, FileTree, Row},
    settings::Settings,
    terminal::{TerminalCommand, TerminalView},
};

gpui_kit::actions!(
    workspace,
    [
        ShowChanges, ShowFiles, ShowProtected, ShowTests, ToggleWrap, NextFile, PreviousFile, NextChange,
        PreviousChange, FindFile, ToggleTerminalFocus, RunAllTests
    ]
);

/// Where single letters navigate: not while typing in the terminal or a
/// search field.
const NAVIGATION: &str = "Workspace && !Terminal && !Command";

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("j", NextFile, Some(NAVIGATION)),
        KeyBinding::new("k", PreviousFile, Some(NAVIGATION)),
        KeyBinding::new("n", NextChange, Some(NAVIGATION)),
        KeyBinding::new("p", PreviousChange, Some(NAVIGATION)),
        KeyBinding::new("ctrl-p", FindFile, Some(NAVIGATION)),
        KeyBinding::new("ctrl-1", ShowChanges, None),
        KeyBinding::new("ctrl-2", ShowFiles, None),
        KeyBinding::new("ctrl-3", ShowProtected, None),
        KeyBinding::new("ctrl-4", ShowTests, None),
        KeyBinding::new("alt-z", ToggleWrap, None),
        KeyBinding::new("ctrl-`", ToggleTerminalFocus, None),
        KeyBinding::new("ctrl-shift-t", RunAllTests, None),
        // The X convention, and what Omarchy's Super+C and Super+V send to
        // windows it takes for terminals.
        KeyBinding::new("ctrl-insert", gpui_kit::component::input::Copy, Some("Root")),
        KeyBinding::new("ctrl-insert", gpui_kit::component::input::Copy, Some("Input")),
        KeyBinding::new("shift-insert", gpui_kit::component::input::Paste, Some("Input")),
    ]);
}

pub enum WorkspaceEvent {
    /// The user wants to go back to the start screen.
    Close,
}

impl EventEmitter<WorkspaceEvent> for Workspace {}

/// Narrowest the diff may get when panels are dragged wide, its minimap
/// (about 100 px) included.
const MIN_DIFF_WIDTH: f32 = 360.;
const MIN_SIDEBAR_WIDTH: f32 = 160.;
const MIN_TERMINAL_WIDTH: f32 = 240.;

/// A divider between panels being dragged.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Divider {
    Sidebar,
    Terminal,
}

/// Files larger than this are not previewed.
const PREVIEW_LIMIT: usize = 2 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
enum SidebarTab {
    Changes,
    Files,
    Protected,
    Tests,
}

impl SidebarTab {
    const ALL: [Self; 4] = [Self::Changes, Self::Files, Self::Protected, Self::Tests];

    fn name(self) -> &'static str {
        match self {
            Self::Changes => "changes",
            Self::Files => "files",
            Self::Protected => "protected",
            Self::Tests => "tests",
        }
    }

    fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|tab| tab.name() == name)
    }
}

/// An unchanged file opened from the tree.
struct Preview {
    path: String,
    state: Option<Entity<DiffState>>,
    message: Option<SharedString>,
}

/// One open repository: what changed against HEAD, its protected files and
/// its tests.
pub struct Workspace {
    backend: Arc<dyn Backend>,
    guard: Guard,
    changes: Vec<Change>,
    /// Lines added and removed in each change, in the same order.
    change_counts: Vec<(usize, usize)>,
    /// Each change's index by path, for lookups from every visible row.
    change_index: HashMap<String, usize>,
    /// Number of changed files below each directory.
    changed_dirs: HashMap<String, usize>,
    /// Changed files that a protection rule covers; committing or reverting
    /// them clears them.
    violations: Vec<String>,
    tree: FileTree,
    /// Every file in the working tree.
    files: Vec<String>,
    files_hash: u64,
    /// Protected and total files below each directory, for its shield.
    protection: HashMap<String, (usize, usize)>,
    /// The files each rule protects, by rule index, for the Protected tab.
    covered: Vec<Vec<String>>,
    expanded: HashSet<String>,
    rows: Rc<Vec<Row>>,
    /// Scrolls the Files tab, to bring a restored file into view.
    tree_scroll: UniformListScrollHandle,
    tab: SidebarTab,
    /// Rules whose covered files are listed in the Protected tab.
    expanded_rules: HashSet<String>,
    /// Repositories folded in the Changes tab of a multi-repository workspace.
    collapsed_repos: HashSet<String>,
    tests: test_panel::Tests,
    /// The file open when this repository was last closed, opened once the
    /// first status shows it still exists.
    restore: Option<String>,
    /// Names this repository for what is kept between sessions.
    state_key: String,
    diff: Entity<DiffState>,
    /// Panel widths and diff layout, saved for the next session.
    settings: Settings,
    dragging: Option<Divider>,
    /// The changed file in the diff and the hash of its patch, when one is.
    shown: Option<(String, u64)>,
    _diff_task: Option<Task<()>>,
    /// Lines added and removed across all changes.
    totals: (usize, usize),
    preview: Option<Preview>,
    _preview_task: Option<Task<()>>,
    terminal: Option<Entity<TerminalView>>,
    main_panel: Option<Entity<main_panel::MainPanel>>,
    /// Holds the keyboard when neither the terminal nor a field does.
    focus_handle: FocusHandle,
    /// The window has the user's attention; notifications wait otherwise.
    window_active: bool,
    /// The first status arrived; changes after it may notify.
    loaded: bool,
    /// A line to scroll to once the file being opened shows.
    pending_line: Option<(String, usize)>,
    finder: Option<finder::Finder>,
    /// The pointer is over the minimap: its view box shows stronger.
    minimap_hover: Rc<std::cell::Cell<bool>>,
    /// While dragging the minimap's view box, where it was grabbed.
    minimap_drag: Rc<std::cell::Cell<Option<f32>>>,
    selected: Option<String>,
    error: Option<SharedString>,
    refreshing: Option<Task<()>>,
    refresh_again: bool,
    _watcher: Option<Box<dyn Any + Send>>,
    _watch_loop: Task<()>,
}

impl Workspace {
    pub fn new(
        backend: Arc<dyn Backend>,
        shell: TerminalCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut error = None;
        let settings = Settings::load();
        let mode = if settings.split { DiffMode::Split } else { DiffMode::Unified };
        // The whole file is shown, so unchanged runs stay unfolded.
        let diff = cx.new(|cx| DiffState::new([], cx).with_mode(mode).with_context_lines(None));

        let (tx, rx) = async_channel::bounded::<()>(1);
        let watcher = match backend.watch(Box::new(move || {
            let _ = tx.try_send(());
        })) {
            Ok(watcher) => Some(watcher),
            Err(err) => {
                error = Some(format!("File watching unavailable: {err}").into());
                None
            }
        };
        let watch_loop = cx.spawn_in(window, async move |this, cx| {
            while rx.recv().await.is_ok() {
                if this.update_in(cx, |this, window, cx| this.refresh(window, cx)).is_err() {
                    break;
                }
            }
        });

        let terminal = match TerminalView::new(shell, window, cx) {
            Ok(view) => Some(view),
            Err(err) => {
                error = Some(format!("Could not start a terminal: {err}").into());
                None
            }
        };
        if let Some(view) = &terminal {
            cx.observe(view, |_, _, cx| cx.notify()).detach();
            view.focus_handle(cx).focus(window, cx);
        }

        let state_key = format!("{}:{}", backend.host().unwrap_or_default(), backend.root());
        let session = session::Session::load(&state_key);
        let tests = test_panel::Tests::load(&state_key);
        let mut this = Self {
            backend,
            guard: Guard::empty(),
            changes: Vec::new(),
            change_counts: Vec::new(),
            change_index: HashMap::new(),
            changed_dirs: HashMap::new(),
            violations: Vec::new(),
            tree: FileTree::default(),
            files: Vec::new(),
            files_hash: 0,
            protection: HashMap::new(),
            covered: Vec::new(),
            expanded: session.expanded.into_iter().collect(),
            rows: Rc::default(),
            tree_scroll: UniformListScrollHandle::new(),
            tab: session.tab.as_deref().and_then(SidebarTab::parse).unwrap_or(SidebarTab::Changes),
            restore: session.selected,
            state_key,
            collapsed_repos: HashSet::new(),
            expanded_rules: HashSet::new(),
            tests,
            diff,
            settings,
            dragging: None,
            shown: None,
            _diff_task: None,
            totals: (0, 0),
            preview: None,
            _preview_task: None,
            terminal,
            main_panel: None,
            focus_handle: cx.focus_handle(),
            window_active: true,
            loaded: false,
            pending_line: None,
            finder: None,
            minimap_hover: Rc::default(),
            minimap_drag: Rc::default(),
            selected: None,
            error,
            refreshing: None,
            refresh_again: false,
            _watcher: watcher,
            _watch_loop: watch_loop,
        };
        let workspace = cx.entity();
        let diff = this.diff.clone();
        this.main_panel = Some(cx.new(|cx| main_panel::MainPanel::new(&workspace, &diff, cx)));
        this.refresh(window, cx);
        this
    }

    /// Everything the center panel shows, so it renders only when this changes.
    fn main_key(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.shown.hash(&mut hasher);
        self.changes.is_empty().hash(&mut hasher);
        (self.settings.soft_wrap, self.settings.split).hash(&mut hasher);
        (self.tests.show_output, self.tests.version).hash(&mut hasher);
        if let Some(preview) = &self.preview {
            preview.path.hash(&mut hasher);
            preview.state.as_ref().map(|state| state.entity_id()).hash(&mut hasher);
            preview.message.hash(&mut hasher);
        }
        // What the diff header of the shown file says about it.
        if let Some(change) = self.shown.as_ref().and_then(|(path, _)| self.change(path)) {
            self.guard.is_protected(&change.path).hash(&mut hasher);
            self.violations.contains(&change.path).hash(&mut hasher);
        }
        hasher.finish()
    }

    /// Recomputes the changes in the background, coalescing bursts.
    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.refreshing.is_some() {
            self.refresh_again = true;
            return;
        }
        let backend = self.backend.clone();
        self.refreshing = Some(cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_executor().spawn(async move { backend.status() }).await;
            this.update_in(cx, |this, window, cx| {
                this.refreshing = None;
                match result {
                    Ok(status) => {
                        this.error = None;
                        this.apply(status, window, cx);
                    }
                    Err(err) => this.error = Some(format!("{err:#}").into()),
                }
                if std::mem::take(&mut this.refresh_again) {
                    this.refresh(window, cx);
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn apply(&mut self, status: Status, window: &mut Window, cx: &mut Context<Self>) {
        let Status { changes, files, rules, rules_error } = status;
        match Guard::new(rules) {
            Ok(guard) => self.guard = guard,
            Err(err) => self.error = Some(format!("Invalid rule in .difftower.toml: {err}").into()),
        }
        if let Some(err) = rules_error {
            self.error = Some(format!("Invalid .difftower.toml: {err}").into());
        }

        self.change_counts = changes.iter().map(|c| line_counts(&c.patch)).collect();
        self.totals = self.change_counts.iter().fold((0, 0), |(a, d), (ca, cd)| (a + ca, d + cd));

        let files_hash = hash(&files.join("\0"));
        if files_hash != self.files_hash {
            self.files_hash = files_hash;
            self.tree = FileTree::new(files.iter().map(String::as_str));
            self.files = files;
            self.rebuild_rows();
        }

        self.changed_dirs.clear();
        for change in &changes {
            for dir in file_tree::ancestors(&change.path) {
                *self.changed_dirs.entry(dir.to_owned()).or_default() += 1;
            }
        }
        self.change_index = changes.iter().enumerate().map(|(ix, c)| (c.path.clone(), ix)).collect();
        self.changes = changes;
        self.show_selected(cx);
        let before: HashSet<String> = self.violations.iter().cloned().collect();
        self.update_violations();
        self.update_protection();
        let new: Vec<&str> = self.violations.iter().filter(|v| !before.contains(*v)).map(String::as_str).collect();
        if self.loaded && !new.is_empty() {
            let names: Vec<&str> = new.iter().map(|p| p.rsplit('/').next().unwrap_or(p)).collect();
            let title = format!("Protected file{} modified", if new.len() == 1 { "" } else { "s" });
            self.notify_user(&title, &names.join(", "));
        }
        self.loaded = true;
        if let Some(path) = self.restore.take()
            && (self.change(&path).is_some() || self.files.contains(&path))
        {
            // Shown in the tree too, its folders unfolded.
            self.expanded.extend(file_tree::ancestors(&path).map(str::to_owned));
            self.rebuild_rows();
            if let Some(ix) = self.rows.iter().position(|row| row.path == path) {
                self.tree_scroll.scroll_to_item(ix, ScrollStrategy::Center);
            }
            self.open(&path, window, cx);
        }
    }

    /// Keeps the open file, the tab and the unfolded folders for next time.
    fn save_session(&self) {
        let mut expanded: Vec<String> = self.expanded.iter().cloned().collect();
        expanded.sort();
        session::Session { selected: self.selected.clone(), tab: Some(self.tab.name().to_owned()), expanded }
            .save(&self.state_key);
    }

    /// A desktop notification, when the window is not in front.
    fn notify_user(&self, title: &str, body: &str) {
        if self.window_active {
            return;
        }
        let name = self.backend.name();
        let _ = std::process::Command::new("notify-send")
            .args(["--app-name=Difftower", &format!("{name}: {title}"), body])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map(|mut child| std::thread::spawn(move || child.wait()));
    }

    /// Opens `path`, scrolled to `line` when given.
    fn open_at(&mut self, path: &str, line: Option<u32>, window: &mut Window, cx: &mut Context<Self>) {
        self.pending_line = line.map(|line| (path.to_owned(), line as usize));
        self.open(path, window, cx);
        // Already showing: the diff will not reload, so scroll now.
        if self.shown.as_ref().is_some_and(|(shown, _)| shown == path) && self.preview.is_none() {
            let diff = self.diff.clone();
            self.scroll_to_pending(&diff, cx);
        }
    }

    /// Scrolls `state` to the pending line if it shows that file.
    fn scroll_to_pending(&mut self, state: &Entity<DiffState>, cx: &mut Context<Self>) {
        let Some((path, line)) = self.pending_line.clone() else { return };
        if !state.read(cx).files().iter().any(|f| f.path() == path.as_str()) {
            return;
        }
        self.pending_line = None;
        state.update(cx, |state, cx| state.scroll_to_line(DiffLinePosition::new(path, DiffSide::Modified, line), cx));
    }

    /// Moves the selection to the next or previous changed file.
    fn step_file(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.changes.is_empty() {
            return;
        }
        let current = self.selected.as_deref().and_then(|path| self.changes.iter().position(|c| c.path == path));
        let next = match (current, forward) {
            (None, true) => 0,
            (None, false) => self.changes.len() - 1,
            (Some(ix), true) => (ix + 1).min(self.changes.len() - 1),
            (Some(ix), false) => ix.saturating_sub(1),
        };
        let path = self.changes[next].path.clone();
        self.open(&path, window, cx);
    }

    fn step_change(&mut self, forward: bool, cx: &mut Context<Self>) {
        let state = match &self.preview {
            Some(preview) => preview.state.clone(),
            None => Some(self.diff.clone()),
        };
        if let Some(state) = state {
            state.update(cx, |state, cx| if forward { state.next_change(cx) } else { state.previous_change(cx) });
        }
    }

    /// Recounts protected files per directory; call when files or rules change.
    fn update_protection(&mut self) {
        self.protection.clear();
        self.covered = vec![Vec::new(); self.guard.patterns().len()];
        for file in &self.files {
            let rule = self.guard.rule_for(file);
            if let Some(ix) = rule {
                self.covered[ix].push(file.clone());
            }
            let protected = usize::from(rule.is_some());
            for dir in file_tree::ancestors(file) {
                let (count, total) = self.protection.entry(dir.to_owned()).or_default();
                *count += protected;
                *total += 1;
            }
        }
    }

    fn update_violations(&mut self) {
        self.violations = self
            .changes
            .iter()
            .filter(|c| {
                self.guard.is_protected(&c.path)
                    || c.old_path.as_deref().is_some_and(|p| self.guard.is_protected(p))
            })
            .map(|c| c.path.clone())
            .collect();
    }

    /// Runs a backend operation off the UI thread, reporting a failure in
    /// the status bar.
    fn in_background(
        &self,
        what: &'static str,
        cx: &mut Context<Self>,
        job: impl FnOnce(&dyn Backend) -> anyhow::Result<()> + Send + 'static,
    ) {
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move { job(&*backend) }).await;
            if let Err(err) = result {
                this.update(cx, |this, cx| {
                    this.error = Some(format!("{what}: {err:#}").into());
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    fn rebuild_rows(&mut self) {
        self.rows = Rc::new(self.tree.rows(&self.expanded));
    }

    fn change(&self, path: &str) -> Option<&Change> {
        self.change_index.get(path).map(|&ix| &self.changes[ix])
    }

    fn revert(&mut self, paths: &[String], window: &mut Window, cx: &mut Context<Self>) {
        let changes: Vec<Change> = paths.iter().filter_map(|p| self.change(p).cloned()).collect();
        let backend = self.backend.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { changes.iter().try_for_each(|change| backend.revert(change)) })
                .await;
            this.update_in(cx, |this, window, cx| {
                if let Err(err) = result {
                    this.error = Some(format!("Could not revert: {err:#}").into());
                }
                this.refresh(window, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Adds or removes a protection rule.
    /// Removes one rule, as written in `.difftower.toml`.
    fn remove_rule(&mut self, rule: &str, cx: &mut Context<Self>) {
        let guard = self.guard.with_rule(rule, false);
        self.set_guard(guard, cx);
    }

    /// Protects or unprotects a file, or a directory with everything in it.
    fn set_protected(&mut self, path: &str, is_dir: bool, protect: bool, cx: &mut Context<Self>) {
        let guard = self.guard.with_protected(path, is_dir, protect);
        self.set_guard(guard, cx);
    }

    fn set_guard(&mut self, guard: anyhow::Result<Guard>, cx: &mut Context<Self>) {
        match guard {
            Ok(guard) => self.guard = guard,
            Err(err) => {
                self.error = Some(format!("Invalid rule: {err}").into());
                return;
            }
        }
        let rules = self.guard.patterns().to_vec();
        self.in_background("Could not save .difftower.toml", cx, move |backend| backend.save_rules(&rules));
        self.update_violations();
        self.update_protection();
        cx.notify();
    }

    fn toggle_dir(&mut self, path: &str, cx: &mut Context<Self>) {
        if !self.expanded.remove(path) {
            self.expanded.insert(path.to_owned());
        }
        self.rebuild_rows();
        self.save_session();
        cx.notify();
    }

    /// Puts the whole selected file, with its changes, in the diff, or
    /// nothing. Leaves the diff alone when its patch is unchanged, and keeps
    /// the scroll position when the same file changed again.
    fn show_selected(&mut self, cx: &mut Context<Self>) {
        let change = self
            .selected
            .as_deref()
            .and_then(|path| self.change(path))
            .filter(|_| self.preview.is_none())
            .cloned();
        let target = change.as_ref().map(|c| (c.path.clone(), hash(&c.patch)));
        if target == self.shown {
            return;
        }
        let same_file = matches!((&target, &self.shown), (Some((a, _)), Some((b, _))) if a == b);
        self.shown = target.clone();
        let Some(change) = change else {
            self._diff_task = None;
            self.diff.update(cx, |diff, cx| diff.set_files([], cx));
            return;
        };
        if !same_file {
            // Nothing from the previous file while this one loads.
            self.diff.update(cx, |diff, cx| diff.set_files([], cx));
        }
        let backend = self.backend.clone();
        self._diff_task = Some(cx.spawn(async move |this, cx| {
            let request = change.clone();
            let full = cx.background_executor().spawn(async move { backend.file_diff(&request) }).await;
            this.update(cx, |this, cx| {
                if this.shown != target {
                    return;
                }
                // The hunks alone still beat nothing.
                let patch = full.as_deref().unwrap_or(&change.patch);
                match DiffFile::parse(patch) {
                    Ok(files) if same_file => this.diff.update(cx, |diff, cx| diff.replace_files(files, cx)),
                    Ok(files) => this.diff.update(cx, |diff, cx| diff.set_files(files, cx)),
                    Err(err) => this.error = Some(format!("Could not parse the diff: {}", err.message()).into()),
                }
                let diff = this.diff.clone();
                this.scroll_to_pending(&diff, cx);
                cx.notify();
            })
            .ok();
        }));
    }

    /// Shows a changed file in the diff, or any other file in a preview.
    fn open(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = Some(path.to_owned());
        self.tests.show_output = false;
        if self.change(path).is_some() {
            self.preview = None;
            self._preview_task = None;
            self.show_selected(cx);
        } else {
            self.open_preview(path, window, cx);
        }
        self.save_session();
        cx.notify();
    }

    fn open_preview(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.preview = Some(Preview { path: path.to_owned(), state: None, message: None });
        let backend = self.backend.clone();
        let path = path.to_owned();
        let file = path.clone();
        self._preview_task = Some(cx.spawn_in(window, async move |this, cx| {
            let content = cx.background_executor().spawn(async move { backend.read_file(&file) }).await;
            this.update(cx, |this, cx| {
                let Some(preview) = this.preview.as_mut().filter(|p| p.path == path) else { return };
                match content {
                    Err(err) => preview.message = Some(format!("Could not read the file: {err:#}").into()),
                    Ok(bytes) if bytes.len() > PREVIEW_LIMIT => {
                        preview.message = Some("File too large to preview".into())
                    }
                    Ok(bytes) if bytes.contains(&0) => preview.message = Some("Binary file".into()),
                    Ok(bytes) => {
                        let text = String::from_utf8_lossy(&bytes);
                        let file = DiffFile::unchanged(path.clone(), &text);
                        let state = cx.new(|cx| DiffState::new([file], cx));
                        let target = state.clone();
                        // Its highlighting arrives later and must show.
                        if let Some(panel) = &this.main_panel {
                            let panel = panel.clone();
                            cx.observe(&state, move |_, _, cx| panel.update(cx, |panel, cx| panel.invalidate(cx))).detach();
                        }
                        preview.state = Some(state);
                        this.scroll_to_pending(&target, cx);
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn close_preview(&mut self, cx: &mut Context<Self>) {
        self.preview = None;
        self._preview_task = None;
        self.show_selected(cx);
        cx.notify();
    }

    fn show_tab(&mut self, tab: SidebarTab, cx: &mut Context<Self>) {
        self.tab = tab;
        if tab == SidebarTab::Tests {
            self.ensure_tests_listed(cx);
        }
        self.save_session();
        cx.notify();
    }

    /// Moves the keyboard between the terminal and the rest of the window,
    /// showing the terminal if it was hidden.
    fn toggle_terminal_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(terminal) = self.terminal.clone() else { return };
        if terminal.focus_handle(cx).is_focused(window) {
            self.focus_workspace(window, cx);
        } else {
            if !self.settings.show_terminal {
                self.toggle_terminal(cx);
            }
            terminal.focus_handle(cx).focus(window, cx);
        }
        cx.notify();
    }

    fn toggle_wrap(&mut self, cx: &mut Context<Self>) {
        self.settings.soft_wrap = !self.settings.soft_wrap;
        self.save_settings();
        cx.notify();
    }

    fn toggle_terminal(&mut self, cx: &mut Context<Self>) {
        self.settings.show_terminal = !self.settings.show_terminal;
        self.save_settings();
        cx.notify();
    }

    fn save_settings(&mut self) {
        if let Err(err) = self.settings.save() {
            self.error = Some(format!("Could not save settings: {err}").into());
        }
    }

    /// Resizes the panel whose divider is being dragged to the pointer.
    fn drag_divider(&mut self, x: Pixels, window: &Window, cx: &mut Context<Self>) {
        let Some(divider) = self.dragging else { return };
        let width = f32::from(window.viewport_size().width);
        let x = f32::from(x);
        let terminal = if self.terminal_visible() { self.settings.terminal_width } else { 0. };
        match divider {
            Divider::Sidebar => {
                let max = (width - terminal - MIN_DIFF_WIDTH).max(MIN_SIDEBAR_WIDTH);
                self.settings.sidebar_width = x.clamp(MIN_SIDEBAR_WIDTH, max);
            }
            Divider::Terminal => {
                let max = (width - self.settings.sidebar_width - MIN_DIFF_WIDTH).max(MIN_TERMINAL_WIDTH);
                self.settings.terminal_width = (width - x).clamp(MIN_TERMINAL_WIDTH, max);
            }
        }
        cx.notify();
    }

    fn end_drag(&mut self, cx: &mut Context<Self>) {
        if self.dragging.take().is_some() {
            self.save_settings();
            cx.notify();
        }
    }

    fn terminal_visible(&self) -> bool {
        self.settings.show_terminal && self.terminal.is_some()
    }

    fn render_divider(&self, divider: Divider, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let active = self.dragging == Some(divider);
        div()
            .id(match divider {
                Divider::Sidebar => "divider-sidebar",
                Divider::Terminal => "divider-terminal",
            })
            .flex_none()
            .w(px(5.))
            .h_full()
            .cursor_col_resize()
            .flex()
            .justify_center()
            .child(div().w(px(1.)).h_full().bg(if active { theme.primary } else { theme.border }))
            .hover(|el| el.bg(theme.primary.opacity(0.25)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    this.dragging = Some(divider);
                    cx.notify();
                }),
            )
    }

    fn toggle_mode(&mut self, cx: &mut Context<Self>) {
        self.diff.update(cx, |diff, cx| {
            let mode = match diff.mode() {
                DiffMode::Unified => DiffMode::Split,
                _ => DiffMode::Unified,
            };
            diff.set_mode(mode, cx);
        });
        self.settings.split = self.diff.read(cx).mode() == DiffMode::Split;
        self.save_settings();
        // The diff element is built here, so this view must render again.
        cx.notify();
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let name = self.backend.name();
        let host = self.backend.host();
        let repo_count = self.backend.repos().len();
        let split = self.diff.read(cx).mode() == DiffMode::Split;
        h_flex()
            .h_10()
            .px_3()
            .gap_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().title_bar)
            .child(
                Button::new("home")
                    .small()
                    .ghost()
                    .icon(Icon::new(Lucide::GitCompare).text_color(cx.theme().primary))
                    .tooltip("Open another repository")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(WorkspaceEvent::Close))),
            )
            .child(div().font_semibold().child(name))
            .when(repo_count > 1, |bar| {
                bar.child(div().text_xs().text_color(cx.theme().muted_foreground).child(format!("{repo_count} repos")))
            })
            .when_some(host, |bar, host| {
                bar.child(
                    h_flex()
                        .gap_1()
                        .px_1p5()
                        .rounded_sm()
                        .text_xs()
                        .bg(cx.theme().secondary)
                        .text_color(cx.theme().secondary_foreground)
                        .child(Icon::new(Lucide::Server).xsmall())
                        .child(host),
                )
            })
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("vs HEAD"),
            )
            .child(div().flex_1())
            .child(
                Button::new("find-file")
                    .small()
                    .ghost()
                    .icon(Icon::new(Lucide::Search))
                    .tooltip("Go to file (Ctrl+P)")
                    .on_click(cx.listener(|this, _, window, cx| this.open_finder(window, cx))),
            )
            .child(if self.tests.running() {
                Button::new("run-tests")
                    .small()
                    .ghost()
                    .icon(Icon::new(Lucide::CircleStop).text_color(cx.theme().danger))
                    .label("Stop tests")
                    .on_click(cx.listener(|this, _, _, cx| this.stop_tests(cx)))
            } else {
                Button::new("run-tests")
                    .small()
                    .ghost()
                    .icon(Icon::new(Lucide::Play).text_color(cx.theme().success))
                    .label("Run tests")
                    .tooltip("Run every test of every repository")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.run_all_tests(cx);
                        this.show_tab(SidebarTab::Tests, cx);
                    }))
            })
            .child(
                Button::new("terminal")
                    .small()
                    .ghost()
                    .icon(IconName::SquareTerminal)
                    .selected(self.settings.show_terminal)
                    .tooltip("Toggle terminal")
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_terminal(cx))),
            )
            .child(
                Button::new("wrap")
                    .small()
                    .ghost()
                    .icon(Icon::new(Lucide::TextWrap))
                    .selected(self.settings.soft_wrap)
                    .tooltip("Wrap long lines (Alt+Z)")
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_wrap(cx))),
            )
            .child(
                Button::new("mode")
                    .small()
                    .ghost()
                    .icon(Icon::new(Lucide::Columns2))
                    .label(if split { "Split" } else { "Unified" })
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_mode(cx))),
            )
    }

    fn render_terminal(&self, terminal: Entity<TerminalView>, cx: &mut Context<Self>) -> impl IntoElement {
        let title = terminal.read(cx).title().unwrap_or_else(|| "Terminal".into());
        v_flex()
            .size_full()
            .min_w_0()
            .overflow_hidden()
            .child(
                h_flex()
                    .h_8()
                    .px_3()
                    .gap_2()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(Icon::new(IconName::SquareTerminal).xsmall())
                    .child(div().min_w_0().truncate().child(title)),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(AnyView::from(terminal).cached(StyleRefinement::default().size_full())),
            )
    }

    /// The bottom bar: protection status at a glance, colored by state.
    fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let test_status = self.render_test_status(cx);
        let theme = cx.theme();
        let rules = self.guard.patterns().len();
        let violated = !self.violations.is_empty();
        let (icon, color, label) = if violated {
            let n = self.violations.len();
            let label = format!("{n} protected file{} modified", if n == 1 { "" } else { "s" });
            (Lucide::ShieldAlert, theme.danger, label)
        } else if rules > 0 {
            let label = format!("Protected files intact · {rules} rule{}", if rules == 1 { "" } else { "s" });
            (Lucide::ShieldCheck, theme.success, label)
        } else {
            (Lucide::Shield, theme.muted_foreground, "No protected files".to_owned())
        };
        let (additions, deletions) = self.totals;

        h_flex()
            .h_7()
            .px_3()
            .gap_3()
            .text_xs()
            .border_t_1()
            .border_color(if violated { theme.danger } else { theme.border })
            .bg(if violated { theme.danger.opacity(0.18) } else { theme.status_bar })
            .text_color(theme.muted_foreground)
            .child(
                h_flex()
                    .id("guard-status")
                    .gap_1p5()
                    .cursor_pointer()
                    .text_color(color)
                    .when(violated, |el| el.font_semibold())
                    .child(Icon::new(icon).xsmall())
                    .child(label)
                    .on_click(cx.listener(|this, _, _, cx| this.show_tab(SidebarTab::Protected, cx))),
            )
            .when(violated, |bar| {
                let paths = self.violations.clone();
                bar.child(
                    Button::new("revert-protected")
                        .xsmall()
                        .danger()
                        .icon(IconName::Undo2)
                        .label("Revert all")
                        .on_click(cx.listener(move |this, _, window, cx| this.revert(&paths, window, cx))),
                )
            })
            .children(test_status)
            .child(div().flex_1())
            .children(self.error.clone().map(|err| div().truncate().text_color(theme.danger).child(err)))
            .when(!self.changes.is_empty(), |bar| {
                bar.child(
                    h_flex()
                        .gap_1()
                        .child(format!("{} file{}", self.changes.len(), if self.changes.len() == 1 { "" } else { "s" }))
                        .child(div().text_color(theme.green).child(format!("+{additions}")))
                        .child(div().text_color(theme.red).child(format!("−{deletions}"))),
                )
            })
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::profile::frame_start();
        self.window_active = window.is_window_active();
        // The side panels keep the width the user gave them; the diff takes
        // the rest. In a window too narrow for all three, the terminal and
        // then the file list give way without changing the saved widths.
        let width = f32::from(window.viewport_size().width);
        let terminal = self.terminal.clone().filter(|_| self.settings.show_terminal);
        let mut sidebar_width = self.settings.sidebar_width.max(MIN_SIDEBAR_WIDTH);
        let mut terminal_width =
            if terminal.is_some() { self.settings.terminal_width.max(MIN_TERMINAL_WIDTH) } else { 0. };
        let overflow = sidebar_width + terminal_width + MIN_DIFF_WIDTH - width;
        if overflow > 0. {
            let from_terminal = overflow.min((terminal_width - MIN_TERMINAL_WIDTH).max(0.));
            terminal_width -= from_terminal;
            sidebar_width = (sidebar_width - (overflow - from_terminal)).max(MIN_SIDEBAR_WIDTH);
        }
        let finder = self.render_finder(window, cx);
        let theme = cx.theme();
        v_flex()
            .size_full()
            .track_focus(&self.focus_handle)
            .key_context("Workspace")
            .bg(theme.background)
            .text_color(theme.foreground)
            .on_action(cx.listener(|this, _: &NextFile, window, cx| this.step_file(true, window, cx)))
            .on_action(cx.listener(|this, _: &PreviousFile, window, cx| this.step_file(false, window, cx)))
            .on_action(cx.listener(|this, _: &NextChange, _, cx| this.step_change(true, cx)))
            .on_action(cx.listener(|this, _: &PreviousChange, _, cx| this.step_change(false, cx)))
            .on_action(cx.listener(|this, _: &FindFile, window, cx| this.open_finder(window, cx)))
            .on_action(cx.listener(|this, _: &ToggleTerminalFocus, window, cx| this.toggle_terminal_focus(window, cx)))
            .on_action(cx.listener(|this, _: &RunAllTests, _, cx| {
                this.run_all_tests(cx);
                this.show_tab(SidebarTab::Tests, cx);
            }))
            .on_action(cx.listener(|this, _: &ShowChanges, _, cx| this.show_tab(SidebarTab::Changes, cx)))
            .on_action(cx.listener(|this, _: &ShowFiles, _, cx| this.show_tab(SidebarTab::Files, cx)))
            .on_action(cx.listener(|this, _: &ShowProtected, _, cx| this.show_tab(SidebarTab::Protected, cx)))
            .on_action(cx.listener(|this, _: &ShowTests, _, cx| this.show_tab(SidebarTab::Tests, cx)))
            .on_action(cx.listener(|this, _: &ToggleWrap, _, cx| this.toggle_wrap(cx)))
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, cx| {
                this.drag_divider(event.position.x, window, cx)
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, cx| this.end_drag(cx)))
            .on_mouse_up_out(MouseButton::Left, cx.listener(|this, _, _, cx| this.end_drag(cx)))
            .child(self.render_toolbar(cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(
                        div()
                            .flex_none()
                            .w(px(sidebar_width))
                            .h_full()
                            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.focus_workspace(window, cx)))
                            .child(self.render_sidebar(cx)),
                    )
                    .child(self.render_divider(Divider::Sidebar, cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .overflow_hidden()
                            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.focus_workspace(window, cx)))
                            .children(
                            self.main_panel
                                .clone()
                                .map(|panel| AnyView::from(panel).cached(StyleRefinement::default().size_full())),
                        ),
                    )
                    .when_some(terminal, |row, terminal| {
                        row.child(self.render_divider(Divider::Terminal, cx)).child(
                            div().flex_none().w(px(terminal_width)).h_full().child(self.render_terminal(terminal, cx)),
                        )
                    }),
            )
            .child(self.render_status_bar(cx))
            .children(finder)
            // Painted last: the end of the frame for DIFFTOWER_PROFILE.
            .child(canvas(|_, _, _| {}, |_, _, _, _| crate::profile::frame_end()).absolute().size_0())
    }
}

/// Lines added and removed in a unified patch.
fn line_counts(patch: &str) -> (usize, usize) {
    patch.lines().fold((0, 0), |(added, removed), line| {
        if line.starts_with('+') && !line.starts_with("+++") {
            (added + 1, removed)
        } else if line.starts_with('-') && !line.starts_with("---") {
            (added, removed + 1)
        } else {
            (added, removed)
        }
    })
}

fn hash(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}
