mod diff_view;
mod sidebar;

use std::{
    collections::{HashMap, HashSet},
    hash::{DefaultHasher, Hash, Hasher},
    rc::Rc,
};

use difftower_core::{Approvals, Change, Guard, Repo, RepoWatcher, TreeId};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Selectable as _, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    diff::{DiffFile, DiffMode, DiffState},
    h_flex,
    resizable::{h_resizable, resizable_panel},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::{
    file_tree::{self, FileTree, Row},
    terminal::TerminalView,
};

gpui_kit::actions!(workspace, [ShowChanges, ShowFiles, ShowProtected]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("ctrl-1", ShowChanges, None),
        KeyBinding::new("ctrl-2", ShowFiles, None),
        KeyBinding::new("ctrl-3", ShowProtected, None),
    ]);
}

/// Files larger than this are not previewed.
const PREVIEW_LIMIT: usize = 2 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
enum SidebarTab {
    Changes,
    Files,
    Protected,
}

/// An unchanged file opened from the tree.
struct Preview {
    path: String,
    state: Option<Entity<DiffState>>,
    message: Option<SharedString>,
}

/// One open repository: what changed against HEAD, and the review state the
/// user builds up while reading it.
pub struct Workspace {
    repo: Repo,
    guard: Guard,
    /// The tree the shown changes were computed against.
    base: Option<TreeId>,
    changes: Vec<Change>,
    /// Number of changed files below each directory.
    changed_dirs: HashMap<String, usize>,
    /// Changed files that a protection rule covers and the user has not accepted.
    violations: Vec<String>,
    /// Changed files that a protection rule covers and the user accepted.
    accepted: Vec<String>,
    approvals: Approvals,
    /// Hash of each file's patch when it was marked reviewed. A file whose
    /// patch changes again drops back to unreviewed.
    reviewed: HashMap<String, u64>,
    tree: FileTree,
    files_hash: u64,
    expanded: HashSet<String>,
    rows: Rc<Vec<Row>>,
    tab: SidebarTab,
    diff: Entity<DiffState>,
    patch_hash: u64,
    preview: Option<Preview>,
    _preview_task: Option<Task<()>>,
    terminal: Option<Entity<TerminalView>>,
    show_terminal: bool,
    selected: Option<String>,
    error: Option<SharedString>,
    refreshing: Option<Task<()>>,
    refresh_again: bool,
    _watcher: Option<RepoWatcher>,
    _watch_loop: Task<()>,
}

impl Workspace {
    pub fn new(repo: Repo, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut error = None;
        let guard = Guard::load(repo.root()).unwrap_or_else(|err| {
            error = Some(format!("Invalid .difftower.toml: {err}").into());
            Guard::empty(repo.root())
        });
        let approvals = Approvals::load(&repo);
        let diff = cx.new(|cx| DiffState::new([], cx).with_mode(DiffMode::Unified));

        let (tx, rx) = async_channel::bounded::<()>(1);
        let watcher = match difftower_core::watch(&repo, move || {
            let _ = tx.try_send(());
        }) {
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

        let terminal = match TerminalView::new(repo.root(), window, cx) {
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

        let mut this = Self {
            repo,
            guard,
            base: None,
            changes: Vec::new(),
            changed_dirs: HashMap::new(),
            violations: Vec::new(),
            accepted: Vec::new(),
            approvals,
            reviewed: HashMap::new(),
            tree: FileTree::default(),
            files_hash: 0,
            expanded: HashSet::new(),
            rows: Rc::default(),
            tab: SidebarTab::Changes,
            diff,
            patch_hash: 0,
            preview: None,
            _preview_task: None,
            terminal,
            show_terminal: true,
            selected: None,
            error,
            refreshing: None,
            refresh_again: false,
            _watcher: watcher,
            _watch_loop: watch_loop,
        };
        this.refresh(window, cx);
        this
    }

    /// Recomputes the changes in the background, coalescing bursts.
    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.refreshing.is_some() {
            self.refresh_again = true;
            return;
        }
        let repo = self.repo.clone();
        self.refreshing = Some(cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let base = repo.head_tree()?;
                    let changes = repo.changes(&base)?;
                    let files = repo.files()?;
                    anyhow::Ok((base, changes, files))
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                this.refreshing = None;
                match result {
                    Ok((base, changes, files)) => {
                        this.base = Some(base);
                        this.apply(changes.files, files, cx);
                    }
                    Err(err) => this.error = Some(err.to_string().into()),
                }
                if std::mem::take(&mut this.refresh_again) {
                    this.refresh(window, cx);
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn apply(&mut self, changes: Vec<Change>, files: Vec<String>, cx: &mut Context<Self>) {
        match Guard::load(self.repo.root()) {
            Ok(guard) => self.guard = guard,
            Err(err) => self.error = Some(format!("Invalid .difftower.toml: {err}").into()),
        }

        let patch: String = changes.iter().map(|c| c.patch.as_str()).collect();
        let patch_hash = hash(&patch);
        if patch_hash != self.patch_hash {
            self.patch_hash = patch_hash;
            match DiffFile::parse(&patch) {
                Ok(files) => self.diff.update(cx, |diff, cx| diff.set_files(files, cx)),
                Err(err) => self.error = Some(format!("Could not parse the diff: {}", err.message()).into()),
            }
        }

        let files_hash = hash(&files.join("\0"));
        if files_hash != self.files_hash {
            self.files_hash = files_hash;
            self.tree = FileTree::new(files.iter().map(String::as_str));
            self.rebuild_rows();
        }

        self.changed_dirs.clear();
        for change in &changes {
            for dir in file_tree::ancestors(&change.path) {
                *self.changed_dirs.entry(dir.to_owned()).or_default() += 1;
            }
        }
        if let Err(err) = self.approvals.retain_current(&changes) {
            self.error = Some(format!("Could not save approvals: {err}").into());
        }
        self.changes = changes;
        self.update_violations();
    }

    fn update_violations(&mut self) {
        let (accepted, violations): (Vec<&Change>, Vec<&Change>) = self
            .changes
            .iter()
            .filter(|c| {
                self.guard.is_protected(&c.path)
                    || c.old_path.as_deref().is_some_and(|p| self.guard.is_protected(p))
            })
            .partition(|c| self.approvals.is_approved(c));
        self.violations = violations.into_iter().map(|c| c.path.clone()).collect();
        self.accepted = accepted.into_iter().map(|c| c.path.clone()).collect();
    }

    /// Accepts the current changes to protected files as intended.
    fn accept(&mut self, paths: &[String], cx: &mut Context<Self>) {
        for path in paths {
            let Some(change) = self.change(path).cloned() else { continue };
            if let Err(err) = self.approvals.approve(&change) {
                self.error = Some(format!("Could not save approvals: {err}").into());
            }
        }
        self.update_violations();
        cx.notify();
    }

    fn revoke(&mut self, path: &str, cx: &mut Context<Self>) {
        if let Err(err) = self.approvals.revoke(path) {
            self.error = Some(format!("Could not save approvals: {err}").into());
        }
        self.update_violations();
        cx.notify();
    }

    fn rebuild_rows(&mut self) {
        self.rows = Rc::new(self.tree.rows(&self.expanded));
    }

    fn is_reviewed(&self, change: &Change) -> bool {
        self.reviewed.get(&change.path) == Some(&hash(&change.patch))
    }

    fn change(&self, path: &str) -> Option<&Change> {
        self.changes.iter().find(|c| c.path == path)
    }

    fn toggle_reviewed(&mut self, path: &str, cx: &mut Context<Self>) {
        let Some(change) = self.change(path) else { return };
        if self.is_reviewed(change) {
            self.reviewed.remove(path);
        } else {
            let patch_hash = hash(&change.patch);
            self.reviewed.insert(path.to_owned(), patch_hash);
            self.diff.update(cx, |diff, cx| diff.set_file_collapsed(path, true, cx));
        }
        cx.notify();
    }

    fn revert(&mut self, paths: &[String], window: &mut Window, cx: &mut Context<Self>) {
        let Some(base) = self.base.clone() else { return };
        for path in paths {
            let Some(change) = self.change(path).cloned() else { continue };
            if let Err(err) = self.repo.revert(&base, &change) {
                self.error = Some(format!("Could not revert {path}: {err}").into());
            }
        }
        self.refresh(window, cx);
    }

    /// Adds or removes a protection rule.
    fn toggle_rule(&mut self, rule: &str, cx: &mut Context<Self>) {
        let enabled = !self.guard.has_rule(rule);
        if let Err(err) = self.guard.set_rule(rule, enabled) {
            self.error = Some(format!("Could not save .difftower.toml: {err}").into());
        }
        self.update_violations();
        cx.notify();
    }

    fn toggle_dir(&mut self, path: &str, cx: &mut Context<Self>) {
        if !self.expanded.remove(path) {
            self.expanded.insert(path.to_owned());
        }
        self.rebuild_rows();
        cx.notify();
    }

    /// Shows a changed file in the diff, or any other file in a preview.
    fn open(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = Some(path.to_owned());
        if self.change(path).is_some() {
            self.preview = None;
            self.diff.update(cx, |diff, cx| {
                diff.set_file_collapsed(path, false, cx);
                diff.scroll_to_file(path, cx);
            });
        } else {
            self.open_preview(path, window, cx);
        }
        cx.notify();
    }

    fn open_preview(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.preview = Some(Preview { path: path.to_owned(), state: None, message: None });
        let full_path = self.repo.root().join(path);
        let path = path.to_owned();
        self._preview_task = Some(cx.spawn_in(window, async move |this, cx| {
            let content = cx
                .background_executor()
                .spawn(async move { std::fs::read(full_path) })
                .await;
            this.update(cx, |this, cx| {
                let Some(preview) = this.preview.as_mut().filter(|p| p.path == path) else { return };
                match content {
                    Err(err) => preview.message = Some(format!("Could not read the file: {err}").into()),
                    Ok(bytes) if bytes.len() > PREVIEW_LIMIT => {
                        preview.message = Some("File too large to preview".into())
                    }
                    Ok(bytes) if bytes.contains(&0) => preview.message = Some("Binary file".into()),
                    Ok(bytes) => {
                        let text = String::from_utf8_lossy(&bytes);
                        let file = DiffFile::unchanged(path.clone(), &text);
                        preview.state = Some(cx.new(|cx| DiffState::new([file], cx)));
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
        cx.notify();
    }

    fn show_tab(&mut self, tab: SidebarTab, cx: &mut Context<Self>) {
        self.tab = tab;
        cx.notify();
    }

    fn toggle_mode(&mut self, cx: &mut Context<Self>) {
        self.diff.update(cx, |diff, cx| {
            let mode = match diff.mode() {
                DiffMode::Unified => DiffMode::Split,
                _ => DiffMode::Unified,
            };
            diff.set_mode(mode, cx);
        });
        // The diff element is built here, so this view must render again.
        cx.notify();
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let name = self
            .repo
            .root()
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let split = self.diff.read(cx).mode() == DiffMode::Split;
        h_flex()
            .h_10()
            .px_3()
            .gap_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().title_bar)
            .child(Icon::new(Lucide::GitCompare).small().text_color(cx.theme().primary))
            .child(div().font_semibold().child(name))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("vs HEAD"),
            )
            .child(div().flex_1())
            .child(
                Button::new("terminal")
                    .small()
                    .ghost()
                    .icon(IconName::SquareTerminal)
                    .selected(self.show_terminal)
                    .tooltip("Toggle terminal")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.show_terminal = !this.show_terminal;
                        cx.notify();
                    })),
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
            .border_l_1()
            .border_color(cx.theme().border)
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
            .child(div().flex_1().min_h_0().child(terminal))
    }

    /// The bottom bar: protection status at a glance, colored by state.
    fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let rules = self.guard.patterns().len();
        let violated = !self.violations.is_empty();
        let (icon, color, label) = if violated {
            let n = self.violations.len();
            let label = format!("{n} protected file{} modified", if n == 1 { "" } else { "s" });
            (Lucide::ShieldAlert, theme.danger, label)
        } else if !self.accepted.is_empty() {
            let n = self.accepted.len();
            (Lucide::ShieldCheck, theme.success, format!("{n} protected change{} accepted", if n == 1 { "" } else { "s" }))
        } else if rules > 0 {
            let label = format!("Protected files intact · {rules} rule{}", if rules == 1 { "" } else { "s" });
            (Lucide::ShieldCheck, theme.success, label)
        } else {
            (Lucide::Shield, theme.muted_foreground, "No protected files".to_owned())
        };
        let (additions, deletions) = self.diff.read(cx).files().iter().fold((0, 0), |(a, d), f| {
            (a + f.additions(), d + f.deletions())
        });

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
                let accept_paths = paths.clone();
                bar.child(
                    Button::new("accept-protected")
                        .xsmall()
                        .success()
                        .icon(IconName::Check)
                        .label("Accept all")
                        .tooltip("These changes were intended")
                        .on_click(cx.listener(move |this, _, _, cx| this.accept(&accept_paths, cx))),
                )
                .child(
                    Button::new("revert-protected")
                        .xsmall()
                        .danger()
                        .icon(IconName::Undo2)
                        .label("Revert all")
                        .on_click(cx.listener(move |this, _, window, cx| this.revert(&paths, window, cx))),
                )
            })
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
        // Initial panel sizes; the resizable group remembers user changes.
        let width = window.viewport_size().width;
        let sidebar_width = (width * 0.2).clamp(px(220.), px(340.));
        let terminal_width = width * 0.4;
        let theme = cx.theme();
        v_flex()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .on_action(cx.listener(|this, _: &ShowChanges, _, cx| this.show_tab(SidebarTab::Changes, cx)))
            .on_action(cx.listener(|this, _: &ShowFiles, _, cx| this.show_tab(SidebarTab::Files, cx)))
            .on_action(cx.listener(|this, _: &ShowProtected, _, cx| this.show_tab(SidebarTab::Protected, cx)))
            .child(self.render_toolbar(cx))
            .child(
                h_resizable("main")
                    .child(resizable_panel().size(sidebar_width).child(self.render_sidebar(cx)))
                    .child(
                        resizable_panel()
                            .size_range(px(240.)..Pixels::MAX)
                            .child(div().size_full().min_w_0().overflow_hidden().child(self.render_main(cx))),
                    )
                    .when_some(self.terminal.clone().filter(|_| self.show_terminal), |group, terminal| {
                        group.child(resizable_panel().size(terminal_width).child(self.render_terminal(terminal, cx)))
                    }),
            )
            .child(self.render_status_bar(cx))
    }
}

fn hash(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}
