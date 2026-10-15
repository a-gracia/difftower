use std::{collections::HashMap, rc::Rc};

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    diff::Diff,
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::Workspace;

struct FileState {
    reviewed: bool,
    /// Protected by any rule.
    protected: bool,
    /// Protected and changed without the user's approval.
    violated: bool,
    /// Protected, with the change accepted by the user.
    accepted: bool,
}

impl Workspace {
    /// The center panel: a previewed file, or the diff of every change.
    pub(super) fn render_main(&self, cx: &mut Context<Self>) -> AnyElement {
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
                        reviewed: self.is_reviewed(c),
                        protected: self.guard.is_protected(&c.path),
                        violated: self.violations.contains(&c.path),
                        accepted: self.accepted.contains(&c.path),
                    };
                    (c.path.clone(), state)
                })
                .collect(),
        );
        let this = cx.entity().downgrade();

        // Actions sit right after the file name, not at the end of the
        // header: the header is as wide as the longest line, so its end can
        // be scrolled out of view.
        // Split columns scroll sideways together in the diff component, so
        // the right side of a long line ends up off screen; wrapping keeps
        // each side within its half.
        Diff::new(&self.diff)
            .soft_wrap(self.settings.soft_wrap || self.settings.split)
            .render_header_title_suffix(move |file, _, cx| {
                let path = file.path().to_string();
                let state = states.get(&path);
                let reviewed = state.is_some_and(|s| s.reviewed);
                let protected = state.is_some_and(|s| s.protected);
                let violated = state.is_some_and(|s| s.violated);
                let accepted = state.is_some_and(|s| s.accepted);
                let (pin, revert, review, accept) = (this.clone(), this.clone(), this.clone(), this.clone());
                let (pin_path, revert_path, review_path, accept_path) =
                    (path.clone(), path.clone(), path.clone(), path.clone());
                h_flex()
                    .gap_1()
                    .when(violated, |el| {
                        el.child(Icon::new(Lucide::ShieldAlert).xsmall().text_color(cx.theme().danger))
                            .child(
                                Button::new("accept")
                                    .xsmall()
                                    .success()
                                    .icon(IconName::Check)
                                    .label("Accept")
                                    .tooltip("This change to a protected file was intended")
                                    .on_click(move |_, _, cx| {
                                        let paths = [accept_path.clone()];
                                        accept.update(cx, |this, cx| this.accept(&paths, cx)).ok();
                                    }),
                            )
                    })
                    .when(accepted, |el| {
                        el.child(Icon::new(Lucide::ShieldCheck).xsmall().text_color(cx.theme().success))
                    })
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
            .into_any_element()
    }
}
