use std::ops::Range;

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

    fn render_changes(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut items: Vec<AnyElement> = Vec::new();
        let repos = self.backend.repos();
        if repos.len() > 1 {
            // Changes arrive ordered by repository; group consecutive ones.
            let mut start = 0;
            while start < self.changes.len() {
                let repo = repo_of(&self.changes[start].path, &repos);
                let end = self.changes[start..]
                    .iter()
                    .position(|c| repo_of(&c.path, &repos) != repo)
                    .map_or(self.changes.len(), |n| start + n);
                items.push(self.render_repo_header(repo, start..end, cx));
                if !self.collapsed_repos.contains(repo) {
                    let prefix = if repo.is_empty() { String::new() } else { format!("{repo}/") };
                    for ix in start..end {
                        items.push(self.render_change(ix, &prefix, cx));
                    }
                }
                start = end;
            }
        } else {
            items.extend((0..self.changes.len()).map(|ix| self.render_change(ix, "", cx)));
        }

        v_flex().id("changes").size_full().py_1().overflow_y_scroll().children(items)
    }

    fn render_repo_header(&self, repo: &str, range: Range<usize>, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let changes = &self.changes[range.clone()];
        let violated = changes.iter().any(|c| self.violations.contains(&c.path));
        let collapsed = self.collapsed_repos.contains(repo);
        let toggle = repo.to_owned();
        h_flex()
            .id(SharedString::from(format!("repo-{repo}")))
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
        let selected = self.selected.as_deref() == Some(&path);
        h_flex()
            .id(("change", ix))
            .h(ROW_HEIGHT)
            .pl(px(if prefix.is_empty() { 8. } else { 8. + 2. * INDENT + 4. }))
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
                el.child(Icon::new(Lucide::ShieldAlert).small().text_color(theme.danger))
            })
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
        let expanded = self.expanded_rules.contains(&pattern);
        let files_label = format!("{} file{}", covered.len(), if covered.len() == 1 { "" } else { "s" });
        let (status, status_color) = if !ok {
            (format!("{} modified", modified.len()), theme.danger)
        } else {
            (files_label.clone(), theme.muted_foreground)
        };
        let toggle = pattern.clone();
        let revert_rule = modified.clone();

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
                    .text_color(if ok { theme.success } else { theme.danger }),
            )
            .child(div().flex_1().min_w_0().truncate().child(pattern.clone()))
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
            let name = path.rsplit('/').next().unwrap_or(path);
            let (open_path, revert_path, unprotect_path) = (path.clone(), path.clone(), path.clone());
            h_flex()
                .h(ROW_HEIGHT)
                .pl(px(8. + 3. * INDENT))
                .pr_3()
                .gap_1p5()
                .text_size(NAME_SIZE)
                .hover(|el| el.bg(theme.list_hover))
                .child(file_icons::icon(name))
                .child(
                    div()
                        .id(("covered", id))
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .cursor_pointer()
                        .when(is_modified, |el| el.text_color(theme.danger))
                        .hover(|el| el.underline())
                        .child(path.clone())
                        .on_click(cx.listener(move |this, _, window, cx| this.open(&open_path, window, cx))),
                )
                .when(is_modified, |el| {
                    el.child(
                        Button::new(("revert-file", id))
                            .xsmall()
                            .ghost()
                            .icon(Icon::new(IconName::Undo2).text_color(theme.danger))
                            .tooltip("Revert this file")
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
            .when(!ok, |el| el.bg(theme.danger.opacity(0.08)))
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
                        .child(
                            Button::new(("revert-rule", ix))
                                .xsmall()
                                .danger()
                                .icon(IconName::Undo2)
                                .label("Revert")
                                .on_click(cx.listener(move |this, _, window, cx| this.revert(&revert_rule, window, cx))),
                        ),
                )
            })
            .into_any_element()
    }
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
fn repo_of<'a>(path: &str, repos: &'a [String]) -> &'a str {
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
