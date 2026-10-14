use std::{
    collections::{HashMap, HashSet},
    hash::{DefaultHasher, Hash, Hasher},
    rc::Rc,
};

use difftower_core::{Change, ChangeStatus, Guard, Repo, RepoWatcher, TreeId};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    diff::{Diff, DiffFile, DiffMode, DiffState},
    h_flex,
    notification::Notification,
    resizable::{h_resizable, resizable_panel},
    v_flex,
};
use gpui_kit::component::{Selectable as _, StyledExt as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::terminal::TerminalView;

/// One open repository: what changed against HEAD, or against the start of
/// a session, and the review state the user builds up while reading it.
pub struct Workspace {
    repo: Repo,
    guard: Guard,
    /// Start of the active session; `None` compares against HEAD.
    session: Option<TreeId>,
    /// The tree the shown changes were computed against.
    base: Option<TreeId>,
    changes: Vec<Change>,
    /// Hash of each file's patch when it was marked reviewed. A file whose
    /// patch changes again drops back to unreviewed.
    reviewed: HashMap<String, u64>,
    /// Protected files already reported, so each violation alerts once.
    reported: HashSet<String>,
    diff: Entity<DiffState>,
    terminal: Option<Entity<TerminalView>>,
    show_terminal: bool,
    patch_hash: u64,
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
        // A session left running when the app closed resumes.
        let session = repo.baseline().ok().flatten();
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
            session,
            base: None,
            changes: Vec::new(),
            reviewed: HashMap::new(),
            reported: HashSet::new(),
            diff,
            terminal,
            show_terminal: true,
            patch_hash: 0,
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
        let session = self.session.clone();
        self.refreshing = Some(cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let base = match session {
                        Some(tree) => tree,
                        None => repo.head_tree()?,
                    };
                    let changes = repo.changes(&base)?;
                    anyhow::Ok((base, changes))
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                this.refreshing = None;
                match result {
                    Ok((base, changes)) => {
                        this.base = Some(base);
                        this.apply(changes.files, window, cx);
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

    fn apply(&mut self, changes: Vec<Change>, window: &mut Window, cx: &mut Context<Self>) {
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

        let violations: HashSet<String> = changes
            .iter()
            .filter(|c| self.is_protected(c))
            .map(|c| c.path.clone())
            .collect();
        for path in violations.difference(&self.reported) {
            window.push_notification(
                Notification::error(format!("An agent changed {path}"))
                    .title("Protected file modified")
                    .autohide(false),
                cx,
            );
        }
        self.reported = violations;
        self.changes = changes;
    }

    fn is_protected(&self, change: &Change) -> bool {
        self.guard.is_protected(&change.path)
            || change.old_path.as_deref().is_some_and(|p| self.guard.is_protected(p))
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

    fn toggle_pin(&mut self, path: &str, cx: &mut Context<Self>) {
        let pinned = self.guard.is_pinned(path);
        if let Err(err) = self.guard.set_pinned(path, !pinned) {
            self.error = Some(format!("Could not save .difftower.toml: {err}").into());
        }
        cx.notify();
    }

    /// Starts, or restarts, a session that accumulates every change made
    /// from now on, regardless of commits.
    fn start_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.repo.reset_baseline() {
            Ok(tree) => self.session = Some(tree),
            Err(err) => self.error = Some(err.to_string().into()),
        }
        self.reported.clear();
        self.refresh(window, cx);
        cx.notify();
    }

    fn end_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(err) = self.repo.clear_baseline() {
            self.error = Some(err.to_string().into());
        }
        self.session = None;
        self.reported.clear();
        self.refresh(window, cx);
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
    }

    fn select(&mut self, path: &str, cx: &mut Context<Self>) {
        self.selected = Some(path.to_owned());
        self.diff.update(cx, |diff, cx| {
            diff.set_file_collapsed(path, false, cx);
            diff.scroll_to_file(path, cx);
        });
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
                    .child(if self.session.is_some() { "session changes" } else { "vs HEAD" }),
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
            .map(|bar| match self.session {
                None => bar.child(
                    Button::new("start-session")
                        .small()
                        .outline()
                        .icon(IconName::Play)
                        .label("Start session")
                        .tooltip("Accumulate every change from now on, even after commits")
                        .on_click(cx.listener(|this, _, window, cx| this.start_session(window, cx))),
                ),
                Some(_) => bar
                    .child(
                        Button::new("restart-session")
                            .small()
                            .ghost()
                            .icon(IconName::RefreshCw)
                            .tooltip("Restart the session from the current files")
                            .on_click(cx.listener(|this, _, window, cx| this.start_session(window, cx))),
                    )
                    .child(
                        Button::new("end-session")
                            .small()
                            .primary()
                            .icon(IconName::Close)
                            .label("End session")
                            .tooltip("Go back to the diff against HEAD")
                            .on_click(cx.listener(|this, _, window, cx| this.end_session(window, cx))),
                    ),
            })
    }

    fn render_alert(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let paths: Vec<String> = self.changes.iter().filter(|c| self.is_protected(c)).map(|c| c.path.clone()).collect();
        if paths.is_empty() {
            return None;
        }
        let theme = cx.theme();
        Some(
            h_flex()
                .px_3()
                .py_2()
                .gap_2()
                .bg(theme.danger.opacity(0.15))
                .border_b_1()
                .border_color(theme.danger)
                .text_sm()
                .child(Icon::new(Lucide::ShieldAlert).text_color(theme.danger))
                .child(
                    div()
                        .flex_1()
                        .truncate()
                        .child(format!("Protected files changed: {}", paths.join(", "))),
                )
                .child(
                    Button::new("revert-protected")
                        .small()
                        .danger()
                        .icon(IconName::Undo2)
                        .label("Revert them")
                        .on_click(cx.listener(move |this, _, window, cx| this.revert(&paths, window, cx))),
                ),
        )
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let reviewed = self.changes.iter().filter(|c| self.is_reviewed(c)).count();
        let rows = self.changes.iter().enumerate().map(|(ix, change)| {
            let path = change.path.clone();
            let (dir, file) = match path.rsplit_once('/') {
                Some((dir, file)) => (format!("{dir}/"), file.to_owned()),
                None => (String::new(), path.clone()),
            };
            let (letter, color) = match change.status {
                ChangeStatus::Added => ("A", theme.green),
                ChangeStatus::Deleted => ("D", theme.red),
                ChangeStatus::Renamed => ("R", theme.blue),
                ChangeStatus::Modified | ChangeStatus::TypeChanged => ("M", theme.yellow),
            };
            let is_reviewed = self.is_reviewed(change);
            let selected = self.selected.as_deref() == Some(&path);
            h_flex()
                .id(("file", ix))
                .px_3()
                .py_1()
                .gap_2()
                .text_sm()
                .cursor_pointer()
                .when(selected, |el| el.bg(theme.list_active))
                .hover(|el| el.bg(theme.list_hover))
                .when(is_reviewed, |el| el.opacity(0.5))
                .child(div().w_3().text_xs().font_semibold().text_color(color).child(letter))
                .child(
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .child(div().flex_none().child(file))
                        .child(div().ml_2().truncate().text_xs().text_color(theme.muted_foreground).child(dir)),
                )
                .when(self.is_protected(change), |el| {
                    el.child(Icon::new(Lucide::ShieldAlert).xsmall().text_color(theme.danger))
                })
                .when(is_reviewed, |el| {
                    el.child(Icon::new(IconName::Check).xsmall().text_color(theme.success))
                })
                .on_click(cx.listener(move |this, _, _, cx| this.select(&path, cx)))
        });

        v_flex()
            .size_full()
            .min_w_0()
            .overflow_hidden()
            .bg(theme.sidebar)
            .child(
                h_flex()
                    .h_8()
                    .px_3()
                    .gap_2()
                    .justify_between()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(div().font_semibold().child(format!("CHANGES ({})", self.changes.len())))
                    .child(format!("{reviewed}/{} reviewed", self.changes.len())),
            )
            .child(
                v_flex()
                    .id("files")
                    .flex_1()
                    .overflow_y_scroll()
                    .children(rows),
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

    fn render_diff(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.changes.is_empty() {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_2()
                .text_color(cx.theme().muted_foreground)
                .child(Icon::new(IconName::CircleCheck).large())
                .child(if self.session.is_some() {
                    "No changes since the session started"
                } else {
                    "Working tree clean"
                })
                .into_any_element();
        }

        struct FileState {
            reviewed: bool,
            pinned: bool,
            protected: bool,
        }
        let states: Rc<HashMap<String, FileState>> = Rc::new(
            self.changes
                .iter()
                .map(|c| {
                    let state = FileState {
                        reviewed: self.is_reviewed(c),
                        pinned: self.guard.is_pinned(&c.path),
                        protected: self.is_protected(c),
                    };
                    (c.path.clone(), state)
                })
                .collect(),
        );
        let this = cx.entity().downgrade();
        let title_states = states.clone();

        Diff::new(&self.diff)
            .render_header_title_suffix(move |file, _, cx| {
                let protected = title_states.get(file.path().as_ref()).is_some_and(|s| s.protected);
                div().when(protected, |el| {
                    el.child(Icon::new(Lucide::ShieldAlert).xsmall().text_color(cx.theme().danger))
                })
            })
            .render_header_suffix(move |file, _, _| {
                let path = file.path().to_string();
                let state = states.get(&path);
                let reviewed = state.is_some_and(|s| s.reviewed);
                let pinned = state.is_some_and(|s| s.pinned);
                let (pin, revert, review) = (this.clone(), this.clone(), this.clone());
                let (pin_path, revert_path, review_path) = (path.clone(), path.clone(), path.clone());
                h_flex()
                    .gap_1()
                    .child(
                        Button::new("pin")
                            .xsmall()
                            .ghost()
                            .icon(Icon::new(if pinned { Lucide::ShieldCheck } else { Lucide::Shield }))
                            .tooltip(if pinned { "Unprotect file" } else { "Protect file from agents" })
                            .on_click(move |_, _, cx| {
                                pin.update(cx, |this, cx| this.toggle_pin(&pin_path, cx)).ok();
                            }),
                    )
                    .child(
                        Button::new("revert")
                            .xsmall()
                            .ghost()
                            .icon(IconName::Undo2)
                            .tooltip("Revert this file")
                            .on_click(move |_, window, cx| {
                                let paths = [revert_path.clone()];
                                revert.update(cx, |this, cx| this.revert(&paths, window, cx)).ok();
                            }),
                    )
                    .child(
                        Button::new("review")
                            .xsmall()
                            .when(reviewed, |b| b.success())
                            .when(!reviewed, |b| b.outline())
                            .icon(IconName::Check)
                            .label("Reviewed")
                            .on_click(move |_, _, cx| {
                                review.update(cx, |this, cx| this.toggle_reviewed(&review_path, cx)).ok();
                            }),
                    )
            })
            .into_any_element()
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Initial panel sizes; the resizable group remembers user changes.
        let width = window.viewport_size().width;
        let sidebar_width = (width * 0.18).clamp(px(200.), px(320.));
        let terminal_width = width * 0.4;
        let theme = cx.theme();
        v_flex()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(self.render_toolbar(cx))
            .children(self.render_alert(cx))
            .child(
                h_resizable("main")
                    .child(resizable_panel().size(sidebar_width).child(self.render_sidebar(cx)))
                    .child(resizable_panel()
                            .size_range(px(240.)..Pixels::MAX)
                            .child(div().size_full().min_w_0().overflow_hidden().child(self.render_diff(cx))))
                    .when_some(self.terminal.clone().filter(|_| self.show_terminal), |group, terminal| {
                        group.child(resizable_panel().size(terminal_width).child(self.render_terminal(terminal, cx)))
                    }),
            )
            .child(
                h_flex()
                    .h_6()
                    .px_3()
                    .gap_3()
                    .text_xs()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().status_bar)
                    .text_color(cx.theme().muted_foreground)
                    .child(self.repo.root().display().to_string())
                    .child(div().flex_1())
                    .children(self.error.clone().map(|err| div().text_color(cx.theme().danger).child(err))),
            )
    }
}

fn hash(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}
