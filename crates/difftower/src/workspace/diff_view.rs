use std::{cell::Cell, collections::HashMap, rc::Rc};

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    diff::{Diff, OverviewKind},
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::Workspace;

struct FileState {
    /// Protected by any rule.
    protected: bool,
    /// Protected and changed.
    violated: bool,
}

impl Workspace {
    /// The center panel: a previewed file, or the diff of every change.
    pub(super) fn render_main(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.tests.show_output {
            return self.render_test_output(cx);
        }
        match &self.preview {
            Some(preview) => self.render_preview(preview, cx),
            None => self.render_diff(cx),
        }
    }

    fn render_preview(&self, preview: &super::Preview, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let body = match (&preview.state, &preview.message) {
            (Some(state), _) => Diff::new(state).header_visible(false).soft_wrap(self.settings.soft_wrap).into_any_element(),
            (None, message) => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(theme.muted_foreground)
                .child(message.clone().unwrap_or_else(|| "Loading…".into()))
                .into_any_element(),
        };
        v_flex()
            .size_full()
            .child(
                h_flex()
                    .h_8()
                    .px_3()
                    .gap_2()
                    .text_sm()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(Icon::new(IconName::File).xsmall().text_color(theme.muted_foreground))
                    .child(div().flex_1().truncate().child(preview.path.clone()))
                    .child(div().text_xs().text_color(theme.muted_foreground).child("unchanged"))
                    .child(
                        Button::new("close-preview")
                            .xsmall()
                            .ghost()
                            .icon(IconName::ArrowLeft)
                            .label("Changes")
                            .on_click(cx.listener(|this, _, _, cx| this.close_preview(cx))),
                    ),
            )
            .child(div().flex_1().min_h_0().child(body))
            .into_any_element()
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
                .child("Working tree clean")
                .into_any_element();
        }
        if self.shown.is_none() {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_2()
                .text_color(cx.theme().muted_foreground)
                .child(Icon::new(IconName::File).large())
                .child("Select a file to see its changes")
                .into_any_element();
        }

        let states: Rc<HashMap<String, FileState>> = Rc::new(
            self.changes
                .iter()
                .map(|c| {
                    let state = FileState {
                        protected: self.guard.is_protected(&c.path),
                        violated: self.violations.contains(&c.path),
                    };
                    (c.path.clone(), state)
                })
                .collect(),
        );
        let this = cx.entity().downgrade();

        // Actions sit right after the file name, not at the end of the
        // header: the header is as wide as the longest line, so its end can
        // be scrolled out of view.
        let diff = Diff::new(&self.diff)
            .soft_wrap(self.settings.soft_wrap)
            .render_header_title_suffix(move |file, _, cx| {
                let path = file.path().to_string();
                let state = states.get(&path);
                let protected = state.is_some_and(|s| s.protected);
                let violated = state.is_some_and(|s| s.violated);
                let (pin, revert) = (this.clone(), this.clone());
                let (pin_path, revert_path) = (path.clone(), path.clone());
                h_flex()
                    .gap_1()
                    .when(violated, |el| {
                        el.child(Icon::new(Lucide::ShieldAlert).xsmall().text_color(cx.theme().danger))
                    })
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
                        Button::new("pin")
                            .xsmall()
                            .ghost()
                            .icon(Icon::new(if protected { Lucide::ShieldCheck } else { Lucide::Shield }))
                            .tooltip(if protected { "Unprotect file" } else { "Protect file from agents" })
                            .on_click(move |_, _, cx| {
                                pin.update(cx, |this, cx| this.set_protected(&pin_path, false, !protected, cx)).ok();
                            }),
                    )
            })
            .into_any_element();
        h_flex()
            .size_full()
            .child(div().flex_1().min_w_0().h_full().child(diff))
            .child(self.render_overview(cx))
            .into_any_element()
    }

    /// A strip beside the diff with a mark where each change is and the part
    /// in view; clicking or dragging on it scrolls there.
    fn render_overview(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (added, removed, modified) = (theme.success, theme.danger, theme.blue);
        let in_view = theme.foreground.opacity(0.12);
        let bounds = Rc::new(Cell::new(Bounds::<Pixels>::default()));
        let state = self.diff.clone();
        let scroll_to = {
            let (bounds, state) = (bounds.clone(), state.clone());
            move |y: Pixels, cx: &mut App| {
                let bounds = bounds.get();
                if bounds.size.height > px(0.) {
                    let fraction = (y - bounds.origin.y) / bounds.size.height;
                    state.update(cx, |state, cx| state.scroll_to_fraction(fraction, cx));
                }
            }
        };
        let drag_to = scroll_to.clone();
        div()
            .id("overview")
            .flex_none()
            .w(px(12.))
            .h_full()
            .border_l_1()
            .border_color(theme.border)
            .cursor_pointer()
            .child(
                canvas(
                    {
                        let bounds = bounds.clone();
                        move |area, _, _| bounds.set(area)
                    },
                    move |area, (), window, cx| {
                        let overview = state.read(cx).overview();
                        let height = area.size.height;
                        let at = |fraction: f32| area.origin.y + height * fraction;
                        let visible = &overview.visible;
                        if visible.end > visible.start {
                            let top = at(visible.start);
                            let box_height = (height * (visible.end - visible.start)).max(px(6.));
                            window.paint_quad(fill(
                                Bounds::new(point(area.origin.x, top), size(area.size.width, box_height)),
                                in_view,
                            ));
                        }
                        for mark in &overview.marks {
                            let color = match mark.kind {
                                OverviewKind::Added => added,
                                OverviewKind::Removed => removed,
                                OverviewKind::Modified => modified,
                            };
                            let mark_height = (height * (mark.end - mark.start)).max(px(2.));
                            window.paint_quad(fill(
                                Bounds::new(
                                    point(area.origin.x + px(3.), at(mark.start)),
                                    size(area.size.width - px(6.), mark_height),
                                ),
                                color,
                            ));
                        }
                    },
                )
                .size_full(),
            )
            .on_mouse_down(MouseButton::Left, move |event, _, cx| scroll_to(event.position.y, cx))
            .on_mouse_move(move |event: &MouseMoveEvent, _, cx| {
                if event.pressed_button == Some(MouseButton::Left) {
                    drag_to(event.position.y, cx);
                }
            })
    }
}