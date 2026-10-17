use std::{cell::Cell, collections::HashMap, rc::Rc};

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{
    ActiveTheme as _, GlobalState, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    diff::{Diff, DiffState, MINIMAP_COLUMNS, OverviewKind},
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
    /// The change only adds lines.
    additions_only: bool,
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
        let minimap = preview.state.clone().map(|state| self.render_minimap(state, cx).into_any_element());
        let theme = cx.theme();
        let body = match (&preview.state, &preview.message) {
            (Some(state), _) => h_flex()
                .size_full()
                .child(
                    div().flex_1().min_w_0().h_full().child(
                        Diff::new(state).header_visible(false).soft_wrap(self.settings.soft_wrap).vertical_scrollbar(false),
                    ),
                )
                .children(minimap)
                .into_any_element(),
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
                .child(if self.session.is_some() {
                    "Nothing changed since the session started"
                } else {
                    "Nothing changed since the last commit"
                })
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
                        additions_only: self.only_additions(&c.path),
                    };
                    (c.path.clone(), state)
                })
                .collect(),
        );
        let this = cx.entity().downgrade();
        let session = self.session.is_some();

        // Actions sit right after the file name, not at the end of the
        // header: the header is as wide as the longest line, so its end can
        // be scrolled out of view.
        let diff = Diff::new(&self.diff)
            .soft_wrap(self.settings.soft_wrap)
            // The minimap beside it scrolls instead.
            .vertical_scrollbar(false)
            // The whole file is shown: one number per row is enough.
            .single_line_number(true)
            .render_header_title_suffix(move |file, _, cx| {
                let path = file.path().to_string();
                let state = states.get(&path);
                let protected = state.is_some_and(|s| s.protected);
                let violated = state.is_some_and(|s| s.violated);
                let additions_only = state.is_some_and(|s| s.additions_only);
                let (pin, revert, accept) = (this.clone(), this.clone(), this.clone());
                let (pin_path, revert_path, accept_path) = (path.clone(), path.clone(), path.clone());
                h_flex()
                    .gap_1()
                    .when(violated, |el| {
                        let color = if additions_only { cx.theme().warning } else { cx.theme().danger };
                        el.child(Icon::new(Lucide::ShieldAlert).xsmall().text_color(color))
                    })
                    .when(session, |el| {
                        el.child(
                            Button::new("accept")
                                .xsmall()
                                .ghost()
                                .icon(Icon::new(IconName::Check))
                                .tooltip("Accept this change")
                                .on_click(move |_, window, cx| {
                                    let paths = [accept_path.clone()];
                                    accept.update(cx, |this, cx| this.accept(&paths, window, cx)).ok();
                                }),
                        )
                    })
                    .child(
                        Button::new("revert")
                            .xsmall()
                            .ghost()
                            .icon(IconName::Undo2)
                            .tooltip("Revert this file; Undo brings it back")
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
            .child(self.render_minimap(self.diff.clone(), cx))
            .into_any_element()
    }

    /// A minimap beside the diff, as in VS Code: the file in miniature,
    /// changed rows tinted, and a box over the part in view. Clicking
    /// centers the view there; dragging the box scrolls with it. A file
    /// taller than the minimap scrolls it along.
    fn render_minimap(&self, state: Entity<DiffState>, cx: &mut Context<Self>) -> impl IntoElement {
        const ROW: f32 = 3.;
        const BLOCK: f32 = 2.;
        const CHAR: f32 = 0.8;
        let theme = cx.theme();
        let (added, removed, modified) = (theme.success, theme.danger, theme.blue);
        let (text, slider, slider_hover) =
            (theme.foreground.opacity(0.45), theme.foreground.opacity(0.08), theme.foreground.opacity(0.16));
        let bounds = Rc::new(Cell::new(Bounds::<Pixels>::default()));
        let (hovered, drag) = (self.minimap_hover.clone(), self.minimap_drag.clone());

        // Where the slider sits for a first row in view: `first * k` pixels
        // from the top, `k` shrinking when the minimap scrolls too.
        let geometry = |total: usize, visible: f32, height: f32| -> f32 {
            let fits = height / ROW;
            if (total as f32) <= fits || (total as f32) <= visible {
                ROW
            } else {
                ROW * (1. - (total as f32 - fits) / (total as f32 - visible))
            }
        };
        let to_first = {
            let (bounds, state) = (bounds.clone(), state.clone());
            move |y: Pixels, grab: f32, cx: &mut App| {
                let area = bounds.get();
                let (total, _, visible) = state.read(cx).minimap_view();
                let k = geometry(total, visible, f32::from(area.size.height));
                let slider_top = f32::from(y - area.origin.y) - grab;
                state.update(cx, |state, cx| state.scroll_to_row(slider_top / k.max(f32::EPSILON), cx));
            }
        };
        let (press, drag_to) = (to_first.clone(), to_first);
        let (press_bounds, press_state, press_drag) = (bounds.clone(), state.clone(), drag.clone());
        let hover_state = state.clone();

        div()
            .id("minimap")
            .flex_none()
            .w(px(MINIMAP_COLUMNS as f32 * CHAR + 8.))
            .h_full()
            .border_l_1()
            .border_color(theme.border)
            .cursor_pointer()
            .on_hover(move |is_hovered, _, cx| {
                hovered.set(*is_hovered);
                hover_state.update(cx, |_, cx| cx.notify());
            })
            .child(
                canvas(
                    {
                        let bounds = bounds.clone();
                        move |area, _, _| bounds.set(area)
                    },
                    {
                        let hovered = self.minimap_hover.clone();
                        move |area, (), window, cx| {
                            // A drag follows the pointer anywhere in the window, and
                            // the diff underneath must not take it as selecting text.
                            let (move_drag, up_drag) = (drag.clone(), drag.clone());
                            window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                                let Some(grab) = move_drag.get() else { return };
                                if !phase.capture() {
                                    return;
                                }
                                if event.pressed_button != Some(MouseButton::Left) {
                                    move_drag.set(None);
                                    return;
                                }
                                drag_to(event.position.y, grab, cx);
                                cx.stop_propagation();
                            });
                            // Releasing ends the drag; everyone else must see it too.
                            window.on_mouse_event(move |_: &MouseUpEvent, phase, _, _| {
                                if phase.capture() {
                                    up_drag.set(None);
                                }
                            });
                            // A press here is a drag, never the start of a text selection.
                            window.on_mouse_event(move |event: &MouseDownEvent, phase, _, cx| {
                                if phase.capture() && area.contains(&event.position) {
                                    GlobalState::suppress_text_selection(cx);
                                }
                            });
                            let (total, first, visible) = state.read(cx).minimap_view();
                            let height = f32::from(area.size.height);
                            let fits = (height / ROW).ceil() as usize + 1;
                            let k = geometry(total, visible, height);
                            // The first row of the minimap: zero unless it scrolls.
                            let offset = (first - first * k / ROW).max(0.);
                            let start = offset.floor() as usize;
                            let rows = state.read(cx).minimap_rows(start..start + fits, cx);
                            let x0 = area.origin.x + px(4.);
                            for (i, row) in rows.iter().enumerate() {
                                let y = area.origin.y + px((start as f32 + i as f32 - offset) * ROW);
                                if let Some(kind) = row.change {
                                    let color = match kind {
                                        OverviewKind::Added => added,
                                        OverviewKind::Removed => removed,
                                        OverviewKind::Modified => modified,
                                    };
                                    window.paint_quad(fill(
                                        Bounds::new(point(area.origin.x, y), size(area.size.width, px(ROW))),
                                        color.opacity(0.22),
                                    ));
                                    window.paint_quad(fill(Bounds::new(point(area.origin.x, y), size(px(2.), px(ROW))), color));
                                }
                                for &(col, len, color) in &row.runs {
                                    window.paint_quad(fill(
                                        Bounds::new(
                                            point(x0 + px(col as f32 * CHAR), y),
                                            size(px((len as f32 * CHAR).max(CHAR)), px(BLOCK)),
                                        ),
                                        color.map_or(text, |c| c.opacity(0.75)),
                                    ));
                                }
                            }
                            if total > 0 && visible < total as f32 {
                                let top = area.origin.y + px(first * k);
                                let box_height = px((visible * ROW).min(height).max(8.));
                                window.paint_quad(fill(
                                    Bounds::new(point(area.origin.x, top), size(area.size.width, box_height)),
                                    if hovered.get() { slider_hover } else { slider },
                                ));
                            }
                        }
                    },
                )
                .size_full(),
            )
            .on_mouse_down(MouseButton::Left, move |event, _, cx| {
                GlobalState::suppress_text_selection(cx);
                let area = press_bounds.get();
                let (total, first, visible) = press_state.read(cx).minimap_view();
                let k = geometry(total, visible, f32::from(area.size.height));
                let y = f32::from(event.position.y - area.origin.y);
                let (top, bottom) = (first * k, first * k + visible * ROW);
                // On the box, it moves with the pointer; elsewhere it centers there.
                let grab = if (top..bottom).contains(&y) { y - top } else { visible * ROW / 2. };
                press_drag.set(Some(grab));
                press(event.position.y, grab, cx);
            })
    }
}