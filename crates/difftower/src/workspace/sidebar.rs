use std::ops::Range;

use difftower_core::{ChangeStatus, dir_rule, file_rule};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Selectable as _, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::{SidebarTab, Workspace};

impl Workspace {
    pub(super) fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match self.tab {
            SidebarTab::Changes => self.render_changes(cx).into_any_element(),
            SidebarTab::Files => self.render_files(cx).into_any_element(),
            SidebarTab::Protected => self.render_protected(cx).into_any_element(),
        };
        let theme = cx.theme();
        v_flex()
            .size_full()
            .min_w_0()
            .overflow_hidden()
            .bg(theme.sidebar)
            .child(
                h_flex()
                    .h_9()
                    .px_2()
                    .gap_1()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(self.tab_button(SidebarTab::Changes, format!("Changes {}", self.changes.len()), cx))
                    .child(self.tab_button(SidebarTab::Files, "Files".into(), cx))
                    .child(self.tab_button(SidebarTab::Protected, "Protected".into(), cx)),
            )
            .child(div().flex_1().min_h_0().child(content))
    }

    fn tab_button(&self, tab: SidebarTab, label: String, cx: &mut Context<Self>) -> Button {
        let id = match tab {
            SidebarTab::Changes => "tab-changes",
            SidebarTab::Files => "tab-files",
            SidebarTab::Protected => "tab-protected",
        };
        Button::new(id)
            .xsmall()
            .ghost()
            .label(label)
            .selected(self.tab == tab)
            .when(tab == SidebarTab::Protected && !self.violations.is_empty(), |b| {
                b.icon(Icon::new(Lucide::ShieldAlert).text_color(cx.theme().danger))
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                this.tab = tab;
                cx.notify();
            }))
    }

    fn render_changes(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let reviewed = self.changes.iter().filter(|c| self.is_reviewed(c)).count();
        let mut items: Vec<AnyElement> = Vec::new();
        if self.backend.repo_count() > 1 {
            // Changes arrive ordered by repository; group consecutive ones.
            let mut start = 0;
            while start < self.changes.len() {
                let repo = repo_of(&self.changes[start].path);
                let end = self.changes[start..]
                    .iter()
                    .position(|c| repo_of(&c.path) != repo)
                    .map_or(self.changes.len(), |n| start + n);
                items.push(self.render_repo_header(repo, start..end, cx));
                if !self.collapsed_repos.contains(repo) {
                    let prefix = format!("{repo}/");
                    for ix in start..end {
                        items.push(self.render_change(ix, &prefix, cx));
                    }
                }
                start = end;
            }
        } else {
            items.extend((0..self.changes.len()).map(|ix| self.render_change(ix, "", cx)));
        }

        v_flex()
            .size_full()
            .child(
                h_flex()
                    .h_7()
                    .px_3()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("{reviewed}/{} reviewed", self.changes.len())),
            )
            .child(v_flex().id("changes").flex_1().overflow_y_scroll().children(items))
    }

    fn render_repo_header(&self, repo: &str, range: Range<usize>, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let changes = &self.changes[range.clone()];
        let reviewed = changes.iter().filter(|c| self.is_reviewed(c)).count();
        let violated = changes.iter().any(|c| self.violations.contains(&c.path));
        let collapsed = self.collapsed_repos.contains(repo);
        let toggle = repo.to_owned();
        h_flex()
            .id(SharedString::from(format!("repo-{repo}")))
            .mt_1()
            .px_2()
            .py_1()
            .gap_1p5()
            .text_xs()
            .cursor_pointer()
            .hover(|el| el.bg(theme.list_hover))
            .child(
                Icon::new(if collapsed { IconName::ChevronRight } else { IconName::ChevronDown })
                    .xsmall()
                    .text_color(theme.muted_foreground),
            )
            .child(div().flex_1().min_w_0().truncate().font_semibold().child(repo.to_owned()))
            .when(violated, |el| el.child(Icon::new(Lucide::ShieldAlert).xsmall().text_color(theme.danger)))
            .child(
                div()
                    .text_color(theme.muted_foreground)
                    .child(format!("{reviewed}/{}", changes.len())),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                if !this.collapsed_repos.remove(&toggle) {
                    this.collapsed_repos.insert(toggle.clone());
                }
                cx.notify();
            }))
            .into_any_element()
    }

    /// One changed file, its directory shown without `prefix`.
    fn render_change(&self, ix: usize, prefix: &str, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let change = &self.changes[ix];
        let path = change.path.clone();
        let relative = path.strip_prefix(prefix).unwrap_or(&path);
        let (dir, file) = match relative.rsplit_once('/') {
            Some((dir, file)) => (format!("{dir}/"), file.to_owned()),
            None => (String::new(), relative.to_owned()),
        };
        let (letter, color) = status_style(change.status, cx);
        let is_reviewed = self.is_reviewed(change);
        let selected = self.selected.as_deref() == Some(&path);
        h_flex()
            .id(("change", ix))
            .pl(if prefix.is_empty() { px(12.) } else { px(26.) })
            .pr_3()
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
            .when(self.violations.contains(&path), |el| {
                el.child(Icon::new(Lucide::ShieldAlert).xsmall().text_color(theme.danger))
            })
            .when(is_reviewed, |el| el.child(Icon::new(IconName::Check).xsmall().text_color(theme.success)))
            .on_click(cx.listener(move |this, _, window, cx| this.open(&path, window, cx)))
            .into_any_element()
    }

    fn render_files(&self, cx: &mut Context<Self>) -> impl IntoElement {
        uniform_list(
            "file-tree",
            self.rows.len(),
            cx.processor(|this, range: Range<usize>, _, cx| {
                range.map(|ix| this.render_tree_row(ix, cx)).collect::<Vec<_>>()
            }),
        )
        .size_full()
        .py_1()
    }

    fn render_tree_row(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let row = &self.rows[ix];
        let path = row.path.clone();
        let rule = if row.is_dir { dir_rule(&path) } else { file_rule(&path) };
        let direct = self.guard.has_rule(&rule);
        // A trailing slash makes `dir/**` rules of ancestors match a directory.
        let protected = if row.is_dir {
            direct || self.guard.is_protected(&format!("{path}/"))
        } else {
            self.guard.is_protected(&path)
        };
        let violated = if row.is_dir {
            let prefix = format!("{path}/");
            self.violations.iter().any(|v| v.starts_with(&prefix))
        } else {
            self.violations.contains(&path)
        };
        let change = (!row.is_dir).then(|| self.change(&path)).flatten();
        let name_color = match change {
            Some(change) => status_style(change.status, cx).1,
            None if row.is_dir && self.changed_dirs.contains_key(&path) => theme.yellow,
            None => theme.foreground,
        };
        let selected = self.selected.as_deref() == Some(&path);

        let (shield_icon, shield_color, tooltip) = if violated {
            (Lucide::ShieldAlert, theme.danger, "Protected · modified by an agent")
        } else if direct {
            (Lucide::ShieldCheck, theme.success, "Protected · click to unprotect")
        } else if protected {
            (Lucide::ShieldCheck, theme.muted_foreground, "Protected by a parent folder rule")
        } else {
            (Lucide::Shield, theme.muted_foreground, if row.is_dir { "Protect folder" } else { "Protect file" })
        };
        let shield_visible = violated || protected;
        let toggle_rule = rule.clone();
        let shield = Button::new(("shield", ix))
            .xsmall()
            .ghost()
            .icon(Icon::new(shield_icon).text_color(shield_color))
            .tooltip(tooltip)
            .when(protected && !direct, |b| b.disabled(true))
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.toggle_rule(&toggle_rule, cx);
            }));

        let is_dir = row.is_dir;
        h_flex()
            .id(("tree-row", ix))
            .group("tree-row")
            .w_full()
            .h_7()
            .pl(px(8. + row.depth as f32 * 14.))
            .pr_1()
            .gap_1p5()
            .text_sm()
            .cursor_pointer()
            .when(selected, |el| el.bg(theme.list_active))
            .hover(|el| el.bg(theme.list_hover))
            .child(
                div().w_3p5().flex_none().when(is_dir, |el| {
                    el.child(
                        Icon::new(if row.expanded { IconName::ChevronDown } else { IconName::ChevronRight })
                            .xsmall()
                            .text_color(theme.muted_foreground),
                    )
                }),
            )
            .child(
                Icon::new(match (is_dir, row.expanded) {
                    (true, true) => IconName::FolderOpen,
                    (true, false) => IconName::FolderClosed,
                    (false, _) => IconName::File,
                })
                .xsmall()
                .text_color(theme.muted_foreground),
            )
            .child(div().flex_1().min_w_0().truncate().text_color(name_color).child(row.name.clone()))
            .when_some(self.changed_dirs.get(&path).filter(|_| is_dir), |el, count| {
                el.child(div().text_xs().text_color(theme.muted_foreground).child(count.to_string()))
            })
            .child(
                div()
                    .flex_none()
                    .when(!shield_visible, |el| el.opacity(0.).group_hover("tree-row", |s| s.opacity(1.)))
                    .child(shield),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                if is_dir {
                    this.toggle_dir(&path, cx);
                } else {
                    this.open(&path, window, cx);
                }
            }))
            .into_any_element()
    }

    fn render_protected(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        if self.guard.patterns().is_empty() {
            return v_flex()
                .size_full()
                .p_4()
                .gap_2()
                .items_center()
                .justify_center()
                .text_sm()
                .text_center()
                .text_color(theme.muted_foreground)
                .child(Icon::new(Lucide::Shield).large())
                .child("Nothing is protected yet.")
                .child("Use the shield next to a file or folder in the Files tab, or add globs to .difftower.toml.")
                .into_any_element();
        }

        let rules = self.guard.patterns().iter().enumerate().map(|(ix, pattern)| {
            let under_rule = |paths: &[String]| -> Vec<String> {
                paths.iter().filter(|p| self.guard.rules_for(p).contains(&ix)).cloned().collect()
            };
            let modified = under_rule(&self.violations);
            let accepted = under_rule(&self.accepted);
            let ok = modified.is_empty();
            let (status, status_color) = if !ok {
                (format!("{} modified", modified.len()), theme.danger)
            } else if !accepted.is_empty() {
                (format!("{} accepted", accepted.len()), theme.success)
            } else {
                ("intact".to_owned(), theme.muted_foreground)
            };
            let remove_rule = pattern.clone();
            let (accept_rule, revert_rule) = (modified.clone(), modified.clone());
            v_flex()
                .px_3()
                .py_2()
                .gap_1()
                .border_b_1()
                .border_color(theme.border)
                .when(!ok, |el| el.bg(theme.danger.opacity(0.08)))
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Icon::new(if ok { Lucide::ShieldCheck } else { Lucide::ShieldAlert })
                                .small()
                                .text_color(if ok { theme.success } else { theme.danger }),
                        )
                        .child(div().flex_1().min_w_0().truncate().text_sm().child(pattern.clone()))
                        .child(div().flex_none().text_xs().text_color(status_color).child(status))
                        .child(
                            Button::new(("remove-rule", ix))
                                .xsmall()
                                .ghost()
                                .icon(IconName::Close)
                                .tooltip("Stop protecting")
                                .on_click(cx.listener(move |this, _, _, cx| this.toggle_rule(&remove_rule, cx))),
                        ),
                )
                .children(modified.into_iter().enumerate().map(|(file_ix, path)| {
                    let id = ix * 10_000 + file_ix;
                    let (open_path, accept_path, revert_path) = (path.clone(), path.clone(), path.clone());
                    h_flex()
                        .pl_6()
                        .gap_1()
                        .text_xs()
                        .child(
                            div()
                                .id(("violation", id))
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .cursor_pointer()
                                .text_color(theme.danger)
                                .hover(|el| el.underline())
                                .child(path)
                                .on_click(cx.listener(move |this, _, window, cx| this.open(&open_path, window, cx))),
                        )
                        .child(
                            Button::new(("accept-file", id))
                                .xsmall()
                                .ghost()
                                .icon(Icon::new(IconName::Check).text_color(theme.success))
                                .tooltip("Accept this change")
                                .on_click(cx.listener(move |this, _, _, cx| this.accept(std::slice::from_ref(&accept_path), cx))),
                        )
                        .child(
                            Button::new(("revert-file", id))
                                .xsmall()
                                .ghost()
                                .icon(Icon::new(IconName::Undo2).text_color(theme.danger))
                                .tooltip("Revert this file")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.revert(std::slice::from_ref(&revert_path), window, cx)
                                })),
                        )
                }))
                .children(accepted.into_iter().enumerate().map(|(file_ix, path)| {
                    let id = ix * 10_000 + file_ix;
                    let (open_path, revoke_path) = (path.clone(), path.clone());
                    h_flex()
                        .pl_6()
                        .gap_1()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(Icon::new(IconName::Check).xsmall().text_color(theme.success))
                        .child(
                            div()
                                .id(("accepted", id))
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .cursor_pointer()
                                .hover(|el| el.underline())
                                .child(path)
                                .on_click(cx.listener(move |this, _, window, cx| this.open(&open_path, window, cx))),
                        )
                        .child(
                            Button::new(("revoke", id))
                                .xsmall()
                                .ghost()
                                .icon(IconName::Close)
                                .tooltip("Undo acceptance")
                                .on_click(cx.listener(move |this, _, _, cx| this.revoke(&revoke_path, cx))),
                        )
                }))
                .when(!ok, |el| {
                    el.child(
                        h_flex()
                            .pl_6()
                            .pt_1()
                            .gap_1()
                            .child(
                                Button::new(("accept-rule", ix))
                                    .xsmall()
                                    .success()
                                    .icon(IconName::Check)
                                    .label("Accept")
                                    .on_click(cx.listener(move |this, _, _, cx| this.accept(&accept_rule, cx))),
                            )
                            .child(
                                Button::new(("revert-rule", ix))
                                    .xsmall()
                                    .danger()
                                    .icon(IconName::Undo2)
                                    .label("Revert")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.revert(&revert_rule, window, cx)
                                    })),
                            ),
                    )
                })
        });

        v_flex().id("protected").size_full().overflow_y_scroll().children(rules).into_any_element()
    }
}

/// The repository a path belongs to in a multi-repository workspace.
fn repo_of(path: &str) -> &str {
    path.split('/').next().unwrap_or(path)
}

fn status_style(status: ChangeStatus, cx: &App) -> (&'static str, Hsla) {
    let theme = cx.theme();
    match status {
        ChangeStatus::Added => ("A", theme.green),
        ChangeStatus::Deleted => ("D", theme.red),
        ChangeStatus::Renamed => ("R", theme.blue),
        ChangeStatus::Modified | ChangeStatus::TypeChanged => ("M", theme.yellow),
    }
}
