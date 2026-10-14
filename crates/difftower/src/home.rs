//! The window's root: a start screen to pick a repository, local or over
//! SSH, then the workspace for it.

use difftower_core::SshTarget;
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    spinner::Spinner,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::{
    open::{self, Opened},
    recent::{self, Recent},
    workspace::{Workspace, WorkspaceEvent},
};

/// A repository being opened.
struct Opening {
    location: String,
    step: SharedString,
    _task: Task<()>,
    _progress: Task<()>,
}

pub struct Home {
    workspace: Option<Entity<Workspace>>,
    input: Entity<InputState>,
    recent: Recent,
    opening: Option<Opening>,
    error: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

impl Home {
    /// Shows the start screen, opening `location` right away when given.
    pub fn new(location: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("~/code/app  or  user@host:/path/to/repo")
        });
        let subscription = cx.subscribe_in(&input, window, |this, input, event: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                let location = input.read(cx).value().trim().to_owned();
                if !location.is_empty() {
                    this.open(location, window, cx);
                }
            }
        });
        let mut this = Self {
            workspace: None,
            input,
            recent: Recent::load(),
            opening: None,
            error: None,
            _subscriptions: vec![subscription],
        };
        match location {
            Some(location) => this.open(location, window, cx),
            None => this.input.focus_handle(cx).focus(window, cx),
        }
        this
    }

    fn open(&mut self, location: String, window: &mut Window, cx: &mut Context<Self>) {
        self.error = None;
        let (tx, rx) = async_channel::unbounded::<String>();
        let progress = cx.spawn(async move |this, cx| {
            while let Ok(step) = rx.recv().await {
                this.update(cx, |this, cx| {
                    if let Some(opening) = &mut this.opening {
                        opening.step = step.into();
                        cx.notify();
                    }
                })
                .ok();
            }
        });
        let job_location = location.clone();
        let task = cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    open::open(&job_location, &|step| {
                        let _ = tx.try_send(step.to_owned());
                    })
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                this.opening = None;
                match result {
                    Ok(opened) => this.show_workspace(opened, window, cx),
                    Err(err) => this.error = Some(format!("{err:#}").into()),
                }
                cx.notify();
            })
            .ok();
        });
        self.opening = Some(Opening { location, step: "Opening…".into(), _task: task, _progress: progress });
        cx.notify();
    }

    fn pick_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open repository".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = paths.await
                && let Some(path) = paths.into_iter().next()
            {
                this.update_in(cx, |this, window, cx| this.open(path.display().to_string(), window, cx)).ok();
            }
        })
        .detach();
    }

    fn show_workspace(&mut self, opened: Opened, window: &mut Window, cx: &mut Context<Self>) {
        self.recent.push(&opened.location);
        if let Err(err) = self.recent.save() {
            self.error = Some(format!("Could not save recent repositories: {err}").into());
        }
        let title = match opened.backend.host() {
            Some(host) => format!("{} — {host} — Difftower", opened.backend.name()),
            None => format!("{} — Difftower", opened.backend.name()),
        };
        window.set_window_title(&title);
        let workspace = cx.new(|cx| Workspace::new(opened.backend, opened.shell, window, cx));
        self._subscriptions.push(cx.subscribe_in(&workspace, window, |this, _, event, window, cx| {
            match event {
                WorkspaceEvent::Close => this.close_workspace(window, cx),
            }
        }));
        self.workspace = Some(workspace);
    }

    fn close_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace = None;
        self.recent = Recent::load();
        window.set_window_title("Difftower");
        self.input.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    fn remove_recent(&mut self, location: &str, cx: &mut Context<Self>) {
        self.recent.remove(location);
        if let Err(err) = self.recent.save() {
            self.error = Some(format!("Could not save recent repositories: {err}").into());
        }
        cx.notify();
    }

    fn render_recent(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let rows = self.recent.entries().iter().enumerate().map(|(ix, location)| {
            let remote = SshTarget::parse(location).filter(|_| !location.starts_with('/'));
            let name = location.rsplit('/').find(|part| !part.is_empty()).unwrap_or(location).to_owned();
            let (open_location, remove_location) = (location.clone(), location.clone());
            h_flex()
                .id(("recent", ix))
                .group("recent-row")
                .w_full()
                .px_3()
                .py_2()
                .gap_3()
                .rounded_md()
                .cursor_pointer()
                .hover(|el| el.bg(theme.list_hover))
                .child(
                    if remote.is_some() { Icon::new(Lucide::Server) } else { Icon::new(IconName::FolderClosed) }
                        .small()
                        .text_color(theme.muted_foreground),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(div().text_sm().font_medium().truncate().child(name))
                        .child(
                            div()
                                .text_xs()
                                .truncate()
                                .text_color(theme.muted_foreground)
                                .child(recent::display(location)),
                        ),
                )
                .child(
                    div().opacity(0.).group_hover("recent-row", |s| s.opacity(1.)).child(
                        Button::new(("remove-recent", ix))
                            .xsmall()
                            .ghost()
                            .icon(IconName::Close)
                            .tooltip("Remove from recent")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.remove_recent(&remove_location, cx);
                            })),
                    ),
                )
                .on_click(cx.listener(move |this, _, window, cx| this.open(open_location.clone(), window, cx)))
        });

        v_flex()
            .gap_1()
            .child(
                div()
                    .px_3()
                    .text_xs()
                    .font_semibold()
                    .text_color(theme.muted_foreground)
                    .child("RECENT"),
            )
            .when(self.recent.entries().is_empty(), |el| {
                el.child(
                    div()
                        .px_3()
                        .py_2()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child("Repositories you open will show up here."),
                )
            })
            .children(rows)
    }

    fn render_home(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let busy = self.opening.is_some();
        v_flex()
            .id("home")
            .size_full()
            .items_center()
            .overflow_y_scroll()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(580.))
                    .px_6()
                    .pt(px(96.))
                    .pb_8()
                    .gap_8()
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(Icon::new(Lucide::GitCompare).large().text_color(theme.primary))
                                    .child(div().text_2xl().font_semibold().child("Difftower")),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(theme.muted_foreground)
                                    .child("Watch what your agents change."),
                            ),
                    )
                    .child(
                        v_flex()
                            .gap_3()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(div().flex_1().child(Input::new(&self.input).disabled(busy)))
                                    .child(
                                        Button::new("open-input")
                                            .primary()
                                            .label("Open")
                                            .disabled(busy)
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                let location = this.input.read(cx).value().trim().to_owned();
                                                if !location.is_empty() {
                                                    this.open(location, window, cx);
                                                }
                                            })),
                                    ),
                            )
                            .child(
                                h_flex()
                                    .gap_2()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child("A local path, or user@host:/path for a repository over SSH.")
                                    .child(div().flex_1())
                                    .child(
                                        Button::new("pick-folder")
                                            .small()
                                            .outline()
                                            .icon(IconName::FolderOpen)
                                            .label("Choose folder…")
                                            .disabled(busy)
                                            .on_click(cx.listener(|this, _, window, cx| this.pick_folder(window, cx))),
                                    ),
                            )
                            .when_some(self.opening.as_ref(), |el, opening| {
                                el.child(
                                    h_flex()
                                        .gap_2()
                                        .px_3()
                                        .py_2()
                                        .rounded_md()
                                        .bg(theme.secondary)
                                        .text_sm()
                                        .child(Spinner::new().small())
                                        .child(div().flex_none().child(opening.step.clone()))
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .text_xs()
                                                .truncate()
                                                .text_color(theme.muted_foreground)
                                                .child(recent::display(&opening.location)),
                                        )
                                        .child(
                                            Button::new("cancel-open")
                                                .xsmall()
                                                .ghost()
                                                .label("Cancel")
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.opening = None;
                                                    cx.notify();
                                                })),
                                        ),
                                )
                            })
                            .when_some(self.error.clone(), |el, error| {
                                el.child(
                                    h_flex()
                                        .gap_2()
                                        .px_3()
                                        .py_2()
                                        .rounded_md()
                                        .items_start()
                                        .bg(theme.danger.opacity(0.12))
                                        .text_sm()
                                        .text_color(theme.danger)
                                        .child(Icon::new(IconName::CircleX).small().flex_none().mt_0p5())
                                        .child(div().flex_1().min_w_0().child(error)),
                                )
                            }),
                    )
                    .child(self.render_recent(cx)),
            )
    }
}

impl Render for Home {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        match &self.workspace {
            Some(workspace) => workspace.clone().into_any_element(),
            None => self.render_home(cx).into_any_element(),
        }
    }
}
