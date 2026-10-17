use std::{ops::Range, rc::Rc};

use difftower_core::ChangeStatus;
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Selectable as _, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::{SidebarTab, Workspace};
use crate::file_icons;

// Sizes from VS Code's explorer and source control views: dense enough to
// show the many files an agent touches at once.
const ROW_HEIGHT: Pixels = px(22.);
const NAME_SIZE: Pixels = px(13.);
const DETAIL_SIZE: Pixels = px(12.);
const INDENT: f32 = 8.;

impl Workspace {
    pub(super) fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match self.tab {
            SidebarTab::Changes => self.render_changes(cx).into_any_element(),
            SidebarTab::Files => self.render_files(cx).into_any_element(),
            SidebarTab::Protected => self.render_protected(cx).into_any_element(),
            SidebarTab::Tests => self.render_tests(cx).into_any_element(),
        };
        let theme = cx.theme();
        v_flex()
            .size_full()
            .min_w_0()
            .overflow_hidden()
            .bg(theme.sidebar)
            .child(
                h_flex()
                    .h_8()
                    .px_2()
                    .gap_1()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(self.tab_button(SidebarTab::Changes, format!("Changes {}", self.changes.len()), cx))
                    .child(self.tab_button(SidebarTab::Files, "Files".into(), cx))
                    .child(self.tab_button(SidebarTab::Protected, "Protected".into(), cx))
                    .child(self.tab_button(SidebarTab::Tests, "Tests".into(), cx)),
            )
            .child(div().flex_1().min_h_0().child(content))
    }

    fn tab_button(&self, tab: SidebarTab, label: String, cx: &mut Context<Self>) -> Button {
        let id = match tab {
            SidebarTab::Changes => "tab-changes",
            SidebarTab::Files => "tab-files",
            SidebarTab::Protected => "tab-protected",
            SidebarTab::Tests => "tab-tests",
        };
        Button::new(id)
            .xsmall()
            .ghost()
            .label(label)
            .selected(self.tab == tab)
            .when(tab == SidebarTab::Protected && !self.violations.is_empty(), |b| {
                b.icon(Icon::new(Lucide::ShieldAlert).text_color(cx.theme().danger))
            })
            .when(tab == SidebarTab::Tests && self.tests.failed() > 0, |b| {
                b.icon(Icon::new(Lucide::CircleX).text_color(cx.theme().danger))
            })
            .on_click(cx.listener(move |this, _, _, cx| this.show_tab(tab, cx)))
    }

    /// The rows of the Changes tab: a header per repository in a
    /// multi-repository workspace, then its changes unless folded.
    fn change_rows(&self) -> Vec<ChangeRow> {
        let repos = self.backend.repos();
        if repos.len() <= 1 {
            return (0..self.changes.len()).map(ChangeRow::Change).collect();
        }
        let mut rows = Vec::new();
        // Changes arrive ordered by repository; group consecutive ones.
        let mut start = 0;
        while start < self.changes.len() {
            let repo = repo_of(&self.changes[start].path, &repos);
            let end = self.changes[start..]
                .iter()
                .position(|c| repo_of(&c.path, &repos) != repo)
                .map_or(self.changes.len(), |n| start + n);
            rows.push(ChangeRow::Header { repo: repo.to_owned(), range: start..end });
            if !self.collapsed_repos.contains(repo) {
                rows.extend((start..end).map(ChangeRow::Change));
            }
            start = end;
        }
        rows
    }

    /// Only the rows in view are built: thousands of changes stay fast.
    fn render_changes(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = Rc::new(self.change_rows());
        let grouped = self.backend.repos().len() > 1;
        uniform_list(
            "changes",
            rows.len(),
            cx.processor(move |this, range: Range<usize>, _, cx| {
                let repos = this.backend.repos();
                range
                    .map(|ix| match &rows[ix] {
                        ChangeRow::Header { repo, range } => this.render_repo_header(repo, range.clone(), cx),
                        ChangeRow::Change(change) => {
                            let repo = repo_of(&this.changes[*change].path, &repos);
                            let prefix = if repo.is_empty() { String::new() } else { format!("{repo}/") };
                            this.render_change(*change, &prefix, grouped, cx)
                        }
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .size_full()
        .py_1()
    }

    fn render_repo_header(&self, repo: &str, range: Range<usize>, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let changes = &self.changes[range.clone()];
        let violated = changes.iter().any(|c| self.violations.contains(&c.path));
        let collapsed = self.collapsed_repos.contains(repo);
        let toggle = repo.to_owned();
        h_flex()
            .id(SharedString::from(format!("repo-{repo}")))
            // A virtual list sizes rows to their content otherwise, pushing
            // the status letter out of the panel.
            .w_full()
            .h(ROW_HEIGHT)
            .pl_1()
            .pr_3()
            .gap_1()
            .text_size(NAME_SIZE)
            .cursor_pointer()
            .hover(|el| el.bg(theme.list_hover))
            .child(
                Icon::new(if collapsed { IconName::ChevronRight } else { IconName::ChevronDown })
                    .small()
                    .text_color(theme.muted_foreground),
            )
            .child(div().flex_1().min_w_0().truncate().font_semibold().child(if repo.is_empty() {
                // The opened directory, a repository itself.
                self.backend.name()
            } else {
                repo.to_owned()
            }))
            .when(violated, |el| el.child(Icon::new(Lucide::ShieldAlert).xsmall().text_color(theme.danger)))
            .child(
                div()
                    .text_size(DETAIL_SIZE)
                    .text_color(theme.muted_foreground)
                    .child(changes.len().to_string()),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                if !this.collapsed_repos.remove(&toggle) {
                    this.collapsed_repos.insert(toggle.clone());
                }
                cx.notify();
            }))
            .into_any_element()
    }

    /// One changed file, its directory shown without `prefix`, indented
    /// under its repository's header when `grouped`.
    fn render_change(&self, ix: usize, prefix: &str, grouped: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let change = &self.changes[ix];
        let path = change.path.clone();
        let relative = path.strip_prefix(prefix).unwrap_or(&path);
        let (dir, file) = match relative.rsplit_once('/') {
            Some((dir, file)) => (format!("{dir}/"), file.to_owned()),
            None => (String::new(), relative.to_owned()),
        };
        let (letter, color) = status_style(change.status, cx);
        let (added, removed) = self.change_counts.get(ix).copied().unwrap_or_default();
        let selected = self.selected.as_deref() == Some(&path);
        let alert = if self.only_additions(&path) { theme.warning } else { theme.danger };
        let (accept_path, revert_path) = (path.clone(), path.clone());
        let actions = h_flex()
            .flex_none()
            .opacity(0.)
            .group_hover("change-row", |s| s.opacity(1.))
            .when(self.session.is_some(), |el| {
                el.child(
                    Button::new(("accept-change", ix))
                        .xsmall()
                        .ghost()
                        .icon(Icon::new(IconName::Check).text_color(theme.success))
                        .tooltip("Accept this change")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.accept(std::slice::from_ref(&accept_path), window, cx)
                        })),
                )
            })
            .child(
                Button::new(("revert-change", ix))
                    .xsmall()
                    .ghost()
                    .icon(Icon::new(IconName::Undo2))
                    .tooltip("Revert this file; Undo brings it back")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.revert(std::slice::from_ref(&revert_path), window, cx)
                    })),
            );
        h_flex()
            .id(("change", ix))
            .group("change-row")
            // A virtual list sizes rows to their content otherwise, pushing
            // the status letter out of the panel.
            .w_full()
            .h(ROW_HEIGHT)
            .pl(px(if grouped { 8. + 2. * INDENT + 4. } else { 8. }))
            .pr_3()
            .gap_1p5()
            .text_size(NAME_SIZE)
            .cursor_pointer()
            .when(selected, |el| el.bg(theme.list_active))
            .hover(|el| el.bg(theme.list_hover))
            .child(file_icons::icon(&file))
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .gap_1p5()
                    .child(div().flex_none().text_color(color).child(file))
                    .child(div().truncate().text_size(DETAIL_SIZE).text_color(theme.muted_foreground).child(dir)),
            )
            .when(self.violations.contains(&path), |el| {
                el.child(Icon::new(Lucide::ShieldAlert).small().text_color(alert))
            })
            .child(actions)
            .child(
                h_flex()
                    .flex_none()
                    .gap_1()
                    .text_size(DETAIL_SIZE)
                    .when(added > 0, |el| el.child(div().text_color(theme.green).child(format!("+{added}"))))
                    .when(removed > 0, |el| el.child(div().text_color(theme.red).child(format!("−{removed}")))),
            )
            .child(div().w_3().flex_none().text_size(DETAIL_SIZE).font_semibold().text_color(color).child(letter))
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
        .track_scroll(&self.tree_scroll)
    }

    fn render_tree_row(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let row = &self.rows[ix];
        let path = row.path.clone();
        // Like a checkbox: a folder is fully, partly or not protected.
        let protection = if row.is_dir {
            match self.protection.get(&path) {
                Some(&(count, total)) if count == total && total > 0 => Protection::Full,
                Some(&(count, _)) if count > 0 => Protection::Partial,
                _ => Protection::None,
            }
        } else if self.guard.is_protected(&path) {
            Protection::Full
        } else {
            Protection::None
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

        let (shield_icon, shield_color, tooltip) = match (violated, protection) {
            (true, _) => (Lucide::ShieldAlert, theme.danger, "Protected · modified by an agent"),
            (false, Protection::Full) => (Lucide::ShieldCheck, theme.success, "Protected · click to unprotect"),
            (false, Protection::Partial) => {
                (Lucide::ShieldHalf, theme.success, "Partly protected · click to protect everything")
            }
            (false, Protection::None) => {
                (Lucide::Shield, theme.muted_foreground, if row.is_dir { "Protect folder" } else { "Protect file" })
            }
        };
        let shield_visible = violated || protection != Protection::None;
        let (toggle_path, is_dir) = (path.clone(), row.is_dir);
        // Partly protected folders become fully protected, like a checkbox.
        let protect = protection != Protection::Full;
        let shield = Button::new(("shield", ix))
            .xsmall()
            .ghost()
            .icon(Icon::new(shield_icon).text_color(shield_color))
            .tooltip(tooltip)
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.set_protected(&toggle_path, is_dir, protect, cx);
            }));

        h_flex()
            .id(("tree-row", ix))
            .group("tree-row")
            .w_full()
            .h(ROW_HEIGHT)
            .pl(px(4. + row.depth as f32 * INDENT))
            .pr_1()
            .gap_1()
            .text_size(NAME_SIZE)
            .cursor_pointer()
            .when(selected, |el| el.bg(theme.list_active))
            .hover(|el| el.bg(theme.list_hover))
            .child(
                div().w_4().flex_none().when(is_dir, |el| {
                    el.child(
                        Icon::new(if row.expanded { IconName::ChevronDown } else { IconName::ChevronRight })
                            .small()
                            .text_color(theme.muted_foreground),
                    )
                }),
            )
            .child(if is_dir {
                Icon::new(if row.expanded { IconName::FolderOpen } else { IconName::FolderClosed })
                    .small()
                    .flex_none()
                    .text_color(theme.muted_foreground)
                    .into_any_element()
            } else {
                file_icons::icon(&row.name).into_any_element()
            })
            .child(div().flex_1().min_w_0().truncate().text_color(name_color).child(row.name.clone()))
            .when_some(self.changed_dirs.get(&path).filter(|_| is_dir), |el, count| {
                el.child(div().text_size(DETAIL_SIZE).text_color(theme.muted_foreground).child(count.to_string()))
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
        let rules: Vec<AnyElement> = (0..self.guard.patterns().len()).map(|ix| self.render_rule(ix, cx)).collect();
        v_flex().id("protected").size_full().overflow_y_scroll().children(rules).into_any_element()
    }

    fn render_rule(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        const LISTED: usize = 200;
        let theme = cx.theme();
        let pattern = self.guard.patterns()[ix].clone();
        let remove_rule = pattern.clone();
        let remove = Button::new(("remove-rule", ix))
            .xsmall()
            .ghost()
            .icon(IconName::Close)
            .tooltip("Remove this rule")
            .on_click(cx.listener(move |this, _, _, cx| this.remove_rule(&remove_rule, cx)));

        if let Some(glob) = pattern.strip_prefix('!') {
            return h_flex()
                .h(ROW_HEIGHT)
                .px_3()
                .gap_2()
                .border_b_1()
                .border_color(theme.border)
                .text_size(NAME_SIZE)
                .text_color(theme.muted_foreground)
                .child(Icon::new(Lucide::ShieldOff).small())
                .child(div().text_size(DETAIL_SIZE).child("except"))
                .child(div().flex_1().min_w_0().truncate().child(glob.to_owned()))
                .child(remove)
                .into_any_element();
        }

        let covered: Vec<&String> = self.covered.get(ix).map(|files| files.iter().collect()).unwrap_or_default();
        let modified: Vec<String> =
            self.violations.iter().filter(|p| self.guard.rule_for(p) == Some(ix)).cloned().collect();
        let ok = modified.is_empty();
        // Only lines added: amber rather than red.
        let additions_only = !ok && modified.iter().all(|p| self.only_additions(p));
        let alert = if additions_only { theme.warning } else { theme.danger };
        let expanded = self.expanded_rules.contains(&pattern);
        let files_label = format!("{} file{}", covered.len(), if covered.len() == 1 { "" } else { "s" });
        let (status, status_color) = if additions_only {
            (format!("{} extended", modified.len()), alert)
        } else if !ok {
            (format!("{} modified", modified.len()), alert)
        } else {
            (files_label.clone(), theme.muted_foreground)
        };
        let toggle = pattern.clone();
        let (accept_rule, revert_rule, ask_rule) = (modified.clone(), modified.clone(), modified.clone());
        let session = self.session.is_some();

        let header = h_flex()
            .id(("rule", ix))
            .h(ROW_HEIGHT)
            .pl_1()
            .pr_3()
            .gap_1()
            .text_size(NAME_SIZE)
            .cursor_pointer()
            .hover(|el| el.bg(theme.list_hover))
            .child(
                Icon::new(if expanded { IconName::ChevronDown } else { IconName::ChevronRight })
                    .small()
                    .text_color(theme.muted_foreground),
            )
            .child(
                Icon::new(if ok { Lucide::ShieldCheck } else { Lucide::ShieldAlert })
                    .small()
                    .text_color(if ok { theme.success } else { alert }),
            )
            // A folder rule reads as a folder.
            .when(pattern.ends_with("/**"), |el| {
                el.child(Icon::new(IconName::FolderClosed).small().text_color(theme.muted_foreground))
            })
            .child(div().flex_1().min_w_0().truncate().child(pattern.strip_suffix("/**").map_or(pattern.clone(), |dir| format!("{dir}/"))))
            .child(div().flex_none().text_size(DETAIL_SIZE).text_color(status_color).child(status))
            .child(remove)
            .on_click(cx.listener(move |this, _, _, cx| {
                if !this.expanded_rules.remove(&toggle) {
                    this.expanded_rules.insert(toggle.clone());
                }
                cx.notify();
            }));

        // Modified files always show; the rest when expanded.
        let listed: Vec<&String> = if expanded {
            covered.iter().copied().take(LISTED).collect()
        } else {
            covered.iter().copied().filter(|f| modified.contains(f)).collect()
        };
        let rows = listed.into_iter().enumerate().map(|(file_ix, path)| {
            let id = ix * 100_000 + file_ix;
            let is_modified = modified.contains(path);
            let file_alert = if self.only_additions(path) { theme.warning } else { theme.danger };
            let name = path.rsplit('/').next().unwrap_or(path);
            let (open_path, accept_path, revert_path, unprotect_path) =
                (path.clone(), path.clone(), path.clone(), path.clone());
            h_flex()
                .h(ROW_HEIGHT)
                .pl(px(8. + 3. * INDENT))
                .pr_3()
                .gap_1p5()
                .text_size(NAME_SIZE)
                .hover(|el| el.bg(theme.list_hover))
                .child(file_icons::icon(name))
                // As in Changes: the name to read, then its folder, dimmed.
                .child(
                    h_flex()
                        .id(("covered", id))
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .gap_1p5()
                        .cursor_pointer()
                        .child(
                            div()
                                .flex_none()
                                .when(is_modified, |el| el.text_color(file_alert))
                                .hover(|el| el.underline())
                                .child(name.to_owned()),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_size(DETAIL_SIZE)
                                .text_color(theme.muted_foreground)
                                .child(path.rsplit_once('/').map_or(String::new(), |(dir, _)| format!("{dir}/"))),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| this.open(&open_path, window, cx))),
                )
                .when(is_modified && session, |el| {
                    el.child(
                        Button::new(("accept-file", id))
                            .xsmall()
                            .ghost()
                            .icon(Icon::new(IconName::Check).text_color(theme.success))
                            .tooltip("Accept this change")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.accept(std::slice::from_ref(&accept_path), window, cx)
                            })),
                    )
                })
                .when(is_modified, |el| {
                    el.child(
                        Button::new(("revert-file", id))
                            .xsmall()
                            .ghost()
                            .icon(Icon::new(IconName::Undo2).text_color(theme.danger))
                            .tooltip("Revert this file; Undo brings it back")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.revert(std::slice::from_ref(&revert_path), window, cx)
                            })),
                    )
                })
                .child(
                    Button::new(("unprotect", id))
                        .xsmall()
                        .ghost()
                        .icon(Icon::new(Lucide::ShieldOff).text_color(theme.muted_foreground))
                        .tooltip("Stop protecting this file")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.set_protected(&unprotect_path, false, false, cx)
                        })),
                )
        });

        v_flex()
            .border_b_1()
            .border_color(theme.border)
            .when(!ok, |el| el.bg(alert.opacity(0.08)))
            .child(header)
            .children(rows)
            .when(expanded && covered.len() > LISTED, |el| {
                el.child(
                    div()
                        .h(ROW_HEIGHT)
                        .pl(px(8. + 3. * INDENT))
                        .text_size(DETAIL_SIZE)
                        .text_color(theme.muted_foreground)
                        .child(format!("and {} more", covered.len() - LISTED)),
                )
            })
            .when(!ok, |el| {
                el.child(
                    h_flex()
                        .pl(px(8. + 3. * INDENT))
                        .py_1()
                        .gap_1()
                        .when(session, |el| {
                            el.child(
                                Button::new(("accept-rule", ix))
                                    .xsmall()
                                    .success()
                                    .icon(IconName::Check)
                                    .label("Accept")
                                    .on_click(cx.listener(move |this, _, window, cx| this.accept(&accept_rule, window, cx))),
                            )
                        })
                        .child(
                            Button::new(("revert-rule", ix))
                                .xsmall()
                                .danger()
                                .icon(IconName::Undo2)
                                .label("Revert")
                                .on_click(cx.listener(move |this, _, window, cx| this.revert(&revert_rule, window, cx))),
                        )
                        .when(self.terminal.is_some(), |el| {
                            el.child(
                                Button::new(("ask-rule", ix))
                                    .xsmall()
                                    .ghost()
                                    .icon(Icon::new(Lucide::Bot))
                                    .label("Ask agent")
                                    .tooltip("Write a request in the terminal for the agent to restore these files; you press Enter")
                                    .on_click(cx.listener(move |this, _, window, cx| this.ask_agent_to_fix(&ask_rule, window, cx))),
                            )
                        }),
                )
            })
            .into_any_element()
    }
}

enum ChangeRow {
    Header { repo: String, range: Range<usize> },
    /// A change, by index.
    Change(usize),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Protection {
    None,
    Partial,
    Full,
}

/// The repository a path belongs to in a multi-repository workspace.
/// The directory of the repository among `repos` that holds `path`; empty
/// for the opened directory itself.
pub(super) fn repo_of<'a>(path: &str, repos: &'a [String]) -> &'a str {
    repos
        .iter()
        .filter(|repo| !repo.is_empty())
        .find(|repo| path.strip_prefix(repo.as_str()).is_some_and(|rest| rest.starts_with('/')))
        .map_or("", String::as_str)
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
